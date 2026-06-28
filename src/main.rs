mod caret;
mod chat;
mod config;
mod control;
mod dirpicker;
mod input;
mod inspect;
mod keymap;
mod layout;
mod settings;
mod skills_install;
mod terminal;
mod wm;

use eframe::egui;
use terminal::Shell;
use wm::WindowManager;

struct App {
    desktop: WindowManager,
    started: bool,
    /// Is the hover-revealed OS title bar currently shown?
    chrome_open: bool,
    /// When the pointer entered the top reveal zone (for the dwell timer).
    chrome_hot_since: Option<f64>,
    /// Agent-dispatch requests from the control pipe thread.
    ctrl: std::sync::mpsc::Receiver<control::CtrlMsg>,
    /// Persisted app settings (terminal font size today). Seeded into egui's
    /// per-context data each frame and read back to capture Ctrl+Scroll/Ctrl+0
    /// zoom changes any pane made.
    settings: config::Settings,
    /// Set when the live font size diverged from `settings`; the change is
    /// written to disk only after a short debounce so a whole scroll gesture
    /// persists once, not once per notch.
    font_dirty_at: Option<std::time::Instant>,
}

/// Wait this long after the last zoom change before writing `settings.json`.
const FONT_SAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);

impl App {
    fn new(ctrl: std::sync::mpsc::Receiver<control::CtrlMsg>) -> Self {
        Self {
            desktop: WindowManager::new().as_desktop(),
            started: false,
            chrome_open: false,
            chrome_hot_since: None,
            ctrl,
            settings: config::Settings::load(),
            font_dirty_at: None,
        }
    }
}

// ---- OS chrome -------------------------------------------------------------
// Native decorations are off (`with_decorations(false)` in `main`); we draw our
// own title bar, revealed only while the pointer dwells at the very top edge of
// the app window, plus an invisible perimeter rim that restores edge-resize.
const CHROME_H: f32 = 30.0; // revealed bar height
const CHROME_REVEAL: f32 = APP_BORDER_W; // the visible border is the hover target...
const CHROME_DWELL: f64 = 0.2; // ...rest on it this long (s) before the bar shows
const CHROME_GRAB: f32 = 5.0; // outer rim that acts as the OS resize handle
const CHROME_BTN_W: f32 = 42.0;
const APP_BORDER_W: f32 = 7.0; // visible frame around the undecorated window
const APP_BORDER: egui::Color32 = CHROME_BG; // frame matches the revealed bar

const CHROME_BG: egui::Color32 = egui::Color32::from_rgb(46, 42, 35);
const CHROME_BORDER: egui::Color32 = egui::Color32::from_rgb(60, 55, 45);
const CHROME_TEXT: egui::Color32 = egui::Color32::from_rgb(222, 222, 212);
const CHROME_DIM: egui::Color32 = egui::Color32::from_rgb(150, 143, 125);
const CHROME_BTN_HOVER: egui::Color32 = egui::Color32::from_rgb(70, 63, 50);
const CHROME_CLOSE_HOVER: egui::Color32 = egui::Color32::from_rgb(196, 43, 28);

impl App {
    /// Hover-revealed replacement for the native title bar. Hidden until the
    /// pointer rests on the painted window border at the top edge (the dwell
    /// keeps a stray brush past it from triggering), then overlays the content.
    fn show_os_chrome(&mut self, ctx: &egui::Context) {
        let screen = ctx.screen_rect();
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if !maximized {
            Self::os_resize_rim(ctx, screen);
            // Visible frame replacing the native border lost with decorations.
            // A layer painter only paints — it registers no widget, so unlike
            // an Area it can span the whole screen without blocking input.
            ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("app_border"),
            ))
            .rect_stroke(
                screen,
                0.0,
                egui::Stroke::new(APP_BORDER_W, APP_BORDER),
                egui::StrokeKind::Inside,
            );
        }

        let (pointer, any_down, now) =
            ctx.input(|i| (i.pointer.latest_pos(), i.pointer.any_down(), i.time));
        match pointer {
            Some(p) if !self.chrome_open => {
                // `!any_down` keeps the bar away while an in-app window is being
                // dragged to the top edge (snap/maximize gestures).
                if p.y <= screen.min.y + CHROME_REVEAL && !any_down {
                    let since = *self.chrome_hot_since.get_or_insert(now);
                    if now - since >= CHROME_DWELL {
                        self.chrome_open = true;
                    }
                } else {
                    self.chrome_hot_since = None;
                }
            }
            Some(p) => {
                self.chrome_hot_since = None;
                if p.y > screen.min.y + CHROME_H + 4.0 {
                    self.chrome_open = false;
                }
            }
            None => {
                self.chrome_open = false;
                self.chrome_hot_since = None;
            }
        }

        let t = ctx.animate_bool(egui::Id::new("os_chrome_slide"), self.chrome_open);
        if t <= 0.0 {
            return;
        }

        egui::Area::new(egui::Id::new("os_chrome"))
            .order(egui::Order::Foreground)
            .movable(false)
            .fixed_pos(screen.min)
            .constrain(false) // content rides above the top edge mid-slide
            .interactable(self.chrome_open)
            .show(ctx, |ui| {
                // Slide in from the top edge: the bar parks above the window
                // and drops down as t goes 0 -> 1 (retracts on close).
                let bar = egui::Rect::from_min_size(
                    egui::pos2(screen.min.x, screen.min.y - (1.0 - t) * CHROME_H),
                    egui::vec2(screen.width(), CHROME_H),
                );
                // When windowed, the topmost rim belongs to the resize handle —
                // keep the bar's interactive rects out of it.
                let rim = if maximized { 0.0 } else { CHROME_GRAB };

                let close_r = egui::Rect::from_min_max(
                    egui::pos2(bar.max.x - CHROME_BTN_W, bar.min.y + rim),
                    egui::pos2(bar.max.x, bar.max.y),
                );
                let max_r = close_r.translate(egui::vec2(-CHROME_BTN_W, 0.0));
                let min_r = max_r.translate(egui::vec2(-CHROME_BTN_W, 0.0));
                let drag_r = egui::Rect::from_min_max(
                    egui::pos2(bar.min.x, bar.min.y + rim),
                    egui::pos2(min_r.min.x, bar.max.y),
                );

                let drag = ui.allocate_rect(drag_r, egui::Sense::click_and_drag());
                let close = ui.allocate_rect(close_r, egui::Sense::click());
                let maxb = ui.allocate_rect(max_r, egui::Sense::click());
                let minb = ui.allocate_rect(min_r, egui::Sense::click());

                if drag.drag_started() && !maximized {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                if drag.double_clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                }
                if minb.clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                if maxb.clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                }
                if close.clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }

                let p = ui.painter();
                p.rect_filled(bar, 0.0, CHROME_BG);
                p.line_segment(
                    [bar.left_bottom(), bar.right_bottom()],
                    egui::Stroke::new(1.0, CHROME_BORDER),
                );
                p.text(
                    egui::pos2(bar.min.x + 12.0, bar.center().y),
                    egui::Align2::LEFT_CENTER,
                    "Foreman",
                    egui::FontId::proportional(13.0),
                    CHROME_DIM,
                );

                for (resp, glyph) in [
                    (&minb, Glyph::Min),
                    (&maxb, Glyph::Max),
                    (&close, Glyph::Close),
                ] {
                    let hovered = resp.hovered();
                    let mut bg = CHROME_BG;
                    if hovered {
                        bg = if glyph == Glyph::Close {
                            CHROME_CLOSE_HOVER
                        } else {
                            CHROME_BTN_HOVER
                        };
                        p.rect_filled(resp.rect, 0.0, bg);
                    }
                    let col = if hovered { CHROME_TEXT } else { CHROME_DIM };
                    chrome_glyph(p, glyph, resp.rect.center(), maximized, col, bg);
                }
            });
    }

    /// Invisible perimeter rim standing in for the resize borders lost with
    /// native decorations. Hover shows the resize cursor; a drag hands the
    /// gesture to the OS via `BeginResize`. The rim claims input, so in-app
    /// windows flush against the app edge don't fight it for the same pixels.
    fn os_resize_rim(ctx: &egui::Context, screen: egui::Rect) {
        use egui::ResizeDirection as Rd;
        let g = CHROME_GRAB;
        let strips = [
            egui::Rect::from_min_max(screen.min, egui::pos2(screen.max.x, screen.min.y + g)),
            egui::Rect::from_min_max(egui::pos2(screen.min.x, screen.max.y - g), screen.max),
            egui::Rect::from_min_max(
                egui::pos2(screen.min.x, screen.min.y + g),
                egui::pos2(screen.min.x + g, screen.max.y - g),
            ),
            egui::Rect::from_min_max(
                egui::pos2(screen.max.x - g, screen.min.y + g),
                egui::pos2(screen.max.x, screen.max.y - g),
            ),
        ];
        // One Area PER strip. An egui Area registers an invisible widget over its
        // whole bounding rect, and any widget covering the pointer's hit area
        // blocks every layer below it — a single Area spanning all four strips
        // has a full-screen bounding rect and swallows all input in the app.
        let names = ["os_rim_top", "os_rim_bottom", "os_rim_left", "os_rim_right"];
        for (i, rect) in strips.iter().enumerate() {
            egui::Area::new(egui::Id::new(names[i]))
                .order(egui::Order::Foreground)
                .movable(false)
                .fixed_pos(rect.min)
                // On an Area's first frame egui assumes a default size and
                // `constrain` (on by default) shoves the origin up/left to fit
                // it on screen. With absolute-rect content that inflates the
                // recorded bounds to origin..strip — for the bottom strip that
                // was the whole bottom half, registering an invisible
                // Foreground widget that swallowed all input under it.
                .constrain(false)
                .default_size(rect.size())
                .show(ctx, |ui| {
                    let resp = ui.allocate_rect(*rect, egui::Sense::drag());
                    let Some(p) = resp.interact_pointer_pos().or_else(|| resp.hover_pos()) else {
                        return;
                    };
                    let dir = match i {
                        0 if p.x <= screen.min.x + g => Rd::NorthWest,
                        0 if p.x >= screen.max.x - g => Rd::NorthEast,
                        0 => Rd::North,
                        1 if p.x <= screen.min.x + g => Rd::SouthWest,
                        1 if p.x >= screen.max.x - g => Rd::SouthEast,
                        1 => Rd::South,
                        2 => Rd::West,
                        _ => Rd::East,
                    };
                    if resp.hovered() || resp.dragged() {
                        ctx.set_cursor_icon(match dir {
                            Rd::North | Rd::South => egui::CursorIcon::ResizeVertical,
                            Rd::East | Rd::West => egui::CursorIcon::ResizeHorizontal,
                            Rd::NorthEast | Rd::SouthWest => egui::CursorIcon::ResizeNeSw,
                            Rd::NorthWest | Rd::SouthEast => egui::CursorIcon::ResizeNwSe,
                        });
                    }
                    if resp.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
                    }
                });
        }
    }
}

/// Which caption button a glyph is painted for.
#[derive(Clone, Copy, PartialEq)]
enum Glyph {
    Min,
    Max,
    Close,
}

/// Paints a Windows-style caption glyph with primitives (no font-glyph
/// dependency). `Max` draws restore (two offset squares) when maximized.
fn chrome_glyph(
    p: &egui::Painter,
    glyph: Glyph,
    c: egui::Pos2,
    maximized: bool,
    col: egui::Color32,
    bg: egui::Color32,
) {
    let s = egui::Stroke::new(1.2, col);
    match glyph {
        Glyph::Min => {
            p.line_segment([c - egui::vec2(4.5, 0.0), c + egui::vec2(4.5, 0.0)], s);
        }
        Glyph::Max if maximized => {
            let back =
                egui::Rect::from_center_size(c + egui::vec2(1.5, -1.5), egui::vec2(7.0, 7.0));
            p.rect_stroke(back, 1.0, s, egui::StrokeKind::Inside);
            let front =
                egui::Rect::from_center_size(c + egui::vec2(-1.5, 1.5), egui::vec2(7.0, 7.0));
            p.rect_filled(front, 1.0, bg);
            p.rect_stroke(front, 1.0, s, egui::StrokeKind::Inside);
        }
        Glyph::Max => {
            p.rect_stroke(
                egui::Rect::from_center_size(c, egui::vec2(9.0, 9.0)),
                1.0,
                s,
                egui::StrokeKind::Inside,
            );
        }
        Glyph::Close => {
            let d = 4.5;
            p.line_segment([c - egui::vec2(d, d), c + egui::vec2(d, d)], s);
            p.line_segment([c - egui::vec2(d, -d), c + egui::vec2(d, -d)], s);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.started {
            // Opt out of egui's built-in zoom so the terminal owns Ctrl+Scroll
            // and Ctrl+0: otherwise egui diverts Ctrl+wheel into a whole-UI zoom
            // (zeroing `smooth_scroll_delta`, so our handler sees nothing) and
            // consumes Ctrl+0/Ctrl+± to scale all chrome. We want only the
            // terminal *text* to resize, handled in `terminal.rs`.
            ctx.options_mut(|o| {
                o.zoom_with_keyboard = false;
                o.input_options.zoom_modifier = egui::Modifiers::NONE;
            });
            // Desktop hosts project windows; each project is its own sandbox.
            let dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let nid = self.desktop.add_project(Shell::PowerShell, dir, &ctx);
            self.desktop.tile_new(nid, None);
            self.started = true;
        }

        while let Ok(msg) = self.ctrl.try_recv() {
            // Drops server-abandoned requests and undoes orphaned spawns; see
            // WindowManager::handle_ctrl for the reply-timeout contract.
            self.desktop.handle_ctrl(msg, &ctx);
        }

        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let mut area = ui.available_rect_before_wrap();
        if !maximized {
            // The painted frame strokes the outer APP_BORDER_W inside the
            // window edge; keep the desktop inside it, not under it.
            area = area.shrink(APP_BORDER_W);
        }
        // Make the persisted font size the live value every pane reads this frame.
        terminal::set_font_size(&ctx, self.settings.font_size);
        self.desktop.show(ui, area, true, egui::Id::new("desktop"));
        // Capture any zoom a pane applied this frame (Ctrl+Scroll / Ctrl+0) and
        // persist it after a debounce so a scroll gesture writes the file once.
        let live = terminal::font_size(&ctx);
        if live != self.settings.font_size {
            self.settings.font_size = live;
            self.font_dirty_at = Some(std::time::Instant::now());
        }
        if let Some(t) = self.font_dirty_at {
            if t.elapsed() >= FONT_SAVE_DEBOUNCE {
                if let Err(e) = self.settings.save() {
                    eprintln!("foreman: could not save settings: {e}");
                }
                self.font_dirty_at = None;
            }
        }
        // Deliver chat now that every Session has pumped this frame: the room
        // reconciles presence and injects each ready member's missed posts (a
        // just-spawned member that wasn't ready when a post arrived gets it on
        // this frame).
        self.desktop.chat_tick();
        // Drive cross-frame `foreman send` settles now that every Session has
        // pumped this frame; pending entries reply when their terminal quiets.
        self.desktop.advance_settles(std::time::Instant::now());

        self.show_os_chrome(&ctx);

        // Also keeps the control-pipe drain alive: serve() has no Context to
        // wake us, so dispatch latency rides on this unconditional repaint.
        // If repainting ever becomes event-driven, hand serve() a Context.
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}

/// Log any panic (message + location + backtrace) to `foreman_panic.log` before
/// the default hook runs. A panic inside the egui/winit callback unwinds across
/// the platform event loop and aborts the process with an opaque exit code, so
/// without this the cause is invisible. Safe to keep; it only writes on a panic.
fn install_panic_logger() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let line = format!("=== foreman panic ===\n{info}\nbacktrace:\n{bt}\n\n");
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("foreman_panic.log")
        {
            use std::io::Write;
            let _ = f.write_all(line.as_bytes());
            let _ = f.flush();
        }
        default(info);
    }));
}

fn main() -> eframe::Result {
    // Subcommand = thin pipe client (`foreman open ...`), no GUI.
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        std::process::exit(control::client_main(&args[1..]));
    }
    install_panic_logger();
    skills_install::install();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || control::serve(control::PIPE, tx));
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_decorations(false),
        ..Default::default()
    };
    eframe::run_native(
        "Foreman",
        opts,
        Box::new(move |_cc| Ok(Box::new(App::new(rx)))),
    )
}
