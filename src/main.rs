mod caret;
mod chat;
mod chat_view;
mod config;
mod confirm;
mod conpty_install;
mod control;
mod dirpicker;
mod emoji_raster;
mod frame;
mod geom;
mod graphics;
mod icons;
mod input;
mod inspect;
mod job;
mod keymap;
mod landing;
mod layout;
mod notify;
mod panel;
mod proc;
mod psreadline;
mod ready;
mod recents;
mod settings;
mod skills_install;
mod terminal;
mod theme;
mod wm;
mod workspace;

use eframe::egui;
use terminal::Shell;
use theme::*;
use wm::WindowManager;

struct App {
    desktop: WindowManager,
    started: bool,
    /// Target state for the hover-revealed OS title bar (drives the slide).
    chrome_open: bool,
    /// When the pointer entered the reveal zone while fully closed (dwell
    /// timer: open only after this ages past `CHROME_OPEN_DWELL`).
    chrome_enter_since: Option<f64>,
    /// When the pointer left the keep-open zone while the bar was open
    /// (coyote timer: close only after this ages past `CHROME_COYOTE`).
    chrome_leave_since: Option<f64>,
    /// Last frame's slide progress (0 closed → 1 open). Used so a mid-close
    /// re-hover over the still-visible bar can re-expand before `t` hits 0.
    chrome_t: f32,
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
    /// Set when the desktop workspace layout changed; written after debounce.
    workspace_dirty_at: Option<std::time::Instant>,
    /// Last time anything happened (input, PTY output, control msg). Drives the
    /// adaptive repaint cadence: fast while recently active, slow when idle.
    last_activity: Option<std::time::Instant>,
    /// Set once the quit confirm was accepted, so the next viewport Close isn't
    /// intercepted again.
    force_quit: bool,
    /// The empty-state landing screen (wordmark + inline picker + session
    /// icons), shown when the desktop is deserted and `landing_enabled`.
    landing: landing::Landing,
    /// Gated behind `FOREMAN_LANDING`: when unset, startup auto-opens a project
    /// and closing the last one quits (today's behavior); when set, an empty
    /// desktop shows the landing instead.
    landing_enabled: bool,
    /// Whether the landing rendered last frame. Its false→true edge re-opens and
    /// re-focuses the landing's picker (whose one-shot focus flag is otherwise
    /// spent after the first appearance).
    landing_shown: bool,
    /// App-global transient notifications (toasts), drawn on top of everything.
    notify: notify::Notifications,
    /// Recent-project MRU (recents.json), fed by the desktop's open drain.
    recents: recents::Recents,
}

/// Wait this long after the last zoom change before writing `settings.json`.
const FONT_SAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);
/// Wait this long after the last structural workspace change before writing
/// `workspace.json` (slightly longer than font — capture walks the full tree).
const WORKSPACE_SAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(600);

impl App {
    fn new(ctrl: std::sync::mpsc::Receiver<control::CtrlMsg>) -> Self {
        Self {
            desktop: WindowManager::new().as_desktop(),
            started: false,
            chrome_open: false,
            chrome_enter_since: None,
            chrome_leave_since: None,
            chrome_t: 0.0,
            ctrl,
            settings: config::Settings::load(),
            font_dirty_at: None,
            workspace_dirty_at: None,
            last_activity: None,
            force_quit: false,
            landing: landing::Landing::new(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            ),
            landing_enabled: std::env::var_os("FOREMAN_LANDING").is_some(),
            landing_shown: false,
            notify: notify::Notifications::new(),
            recents: recents::Recents::load(),
        }
    }

    /// Capture the live desktop tree and write `workspace.json` immediately.
    /// Clears any pending debounce so a later frame does not double-write.
    fn flush_workspace(&mut self) {
        let snap = self.desktop.capture_workspace();
        if let Err(e) = snap.save() {
            eprintln!("foreman: could not save workspace: {e}");
        }
        self.workspace_dirty_at = None;
    }
}

// ---- OS chrome -------------------------------------------------------------
// Native decorations are off (`with_decorations(false)` in `main`); we draw our
// own title bar, revealed after a short dwell on the top-edge border, held open
// by a longer coyote timer after leave, plus an invisible perimeter rim that
// restores edge-resize.
const CHROME_H: f32 = 30.0; // revealed bar height
/// Vertical depth of the top-edge open zone (from the window top). A few px
/// past the painted border so the strip is easier to hit, but short of the
/// project/terminal titleband so the in-app ✕ isn't stolen.
const CHROME_REVEAL: f32 = 10.0;
const CHROME_KEEP: f32 = CHROME_H + 4.0; // depth that keeps/reopens the bar
const CHROME_COYOTE: f64 = 0.25; // seconds after leave before close starts
/// Dwell before first reveal — 150ms so a pass-through doesn't pop the bar;
/// mid-close re-hover still opens immediately (see state machine).
const CHROME_OPEN_DWELL: f64 = 0.15;
const CHROME_GRAB: f32 = 5.0; // outer rim that acts as the OS resize handle
const CHROME_BTN_W: f32 = 42.0;
const APP_BORDER_W: f32 = 7.0; // visible frame around the undecorated window

impl App {
    /// Hover-revealed replacement for the native title bar. Opens after a short
    /// dwell on the top painted border (skips pass-throughs); stays up for a
    /// coyote grace after leave so a brief miss doesn't retract it; re-hover
    /// mid-close reverses the slide smoothly with no extra dwell.
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
        // Prior-frame slide progress: while the bar is still visible mid-close,
        // the full keep zone (not just the thin reveal strip) counts as hot so
        // re-entering reverse-opens the animation.
        let anim_t = self.chrome_t;
        let in_zone = |p: egui::Pos2, depth: f32| p.y <= screen.min.y + depth;
        let hot = match pointer {
            Some(p) if self.chrome_open => in_zone(p, CHROME_KEEP),
            // `!any_down` keeps the bar away while an in-app window is dragged
            // to the top edge (snap/maximize gestures).
            Some(p) if anim_t > 0.0 => in_zone(p, CHROME_KEEP) && !any_down,
            Some(p) => in_zone(p, CHROME_REVEAL) && !any_down,
            None => false,
        };
        if hot {
            self.chrome_leave_since = None;
            // Already open, or still sliding closed: open immediately so the
            // retracting bar is catchable. Only the fully-closed first reveal
            // pays the open dwell.
            if self.chrome_open || anim_t > 0.0 {
                self.chrome_open = true;
                self.chrome_enter_since = None;
            } else {
                let since = *self.chrome_enter_since.get_or_insert(now);
                if now - since >= CHROME_OPEN_DWELL {
                    self.chrome_open = true;
                    self.chrome_enter_since = None;
                } else {
                    // Idle frames would otherwise stall the dwell clock.
                    ctx.request_repaint();
                }
            }
        } else if self.chrome_open {
            self.chrome_enter_since = None;
            let since = *self.chrome_leave_since.get_or_insert(now);
            if now - since >= CHROME_COYOTE {
                self.chrome_open = false;
                self.chrome_leave_since = None;
            } else {
                // Idle frames would otherwise stall the coyote clock.
                ctx.request_repaint();
            }
        } else {
            self.chrome_enter_since = None;
            self.chrome_leave_since = None;
        }

        let t = ctx.animate_bool(egui::Id::new("os_chrome_slide"), self.chrome_open);
        self.chrome_t = t;
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
                    DIM,
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
                    let col = if hovered { TEXT } else { DIM };
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
            // Restore prior layout when possible; otherwise auto-open cwd project
            // unless the landing screen owns the empty desktop.
            let snap = workspace::WorkspaceSnapshot::load();
            let mut restored = false;
            if !snap.is_empty() {
                let rep = self.desktop.apply_workspace(&snap, &ctx);
                restored = rep.projects_restored > 0;
                if rep.projects_skipped > 0 {
                    eprintln!(
                        "foreman: restored {} project(s), skipped {}",
                        rep.projects_restored, rep.projects_skipped
                    );
                }
            }
            // Task-manager panel is always present (right-edge leaf); not in the
            // workspace snapshot — prefs come from settings.json.
            self.desktop
                .ensure_panel(self.settings.panel_collapsed, self.settings.panel_width);
            if !restored && !self.landing_enabled {
                let dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let nid = self.desktop.add_project(Shell::PowerShell, dir, &ctx);
                self.desktop.tile_new(nid, None);
            }
            // Auto-project / restore must not pollute recents (spec).
            let _ = self.desktop.take_opened();
            // Do not leave restore-induced dirty true on first frame.
            let _ = self.desktop.poll_workspace_dirty();
            self.started = true;
        }

        let mut ctrl_activity = false;
        while let Ok(msg) = self.ctrl.try_recv() {
            // Drops server-abandoned requests and undoes orphaned spawns; see
            // WindowManager::handle_ctrl for the reply-timeout contract.
            self.desktop.handle_ctrl(msg, &ctx);
            ctrl_activity = true;
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
        if self.landing_enabled && self.desktop.deserted() {
            if !self.landing_shown {
                self.landing.reopen(); // re-focus the field each time we land here
                self.landing_shown = true;
            }
            if let Some(act) = self.landing.show(ui, area, self.recents.entries()) {
                match act.kind.launch_command() {
                    // Terminal: a plain shell, as before.
                    None => {
                        let nid = self.desktop.add_project(Shell::PowerShell, act.path, &ctx);
                        self.desktop.tile_new(nid, None);
                    }
                    // Claude/Codex, installed: a normal shell that runs the agent.
                    Some(cmd) if act.kind.installed() => {
                        let nid = self.desktop.add_project_with_command(act.path, cmd, &ctx);
                        self.desktop.tile_new(nid, None);
                    }
                    // Claude/Codex, missing: an error toast; stay on the landing.
                    Some(_) => self.notify.push(
                        notify::Level::Error,
                        format!("{} isn't installed", act.kind.label()),
                        std::time::Instant::now(),
                    ),
                }
            }
        } else {
            self.landing_shown = false;
            self.desktop
                .show(ui, area, true, egui::Id::new("desktop"), false);
        }
        // Quit guard: the window's title-bar X and Alt+F4 send
        // ViewportCommand::Close straight to the viewport, bypassing every WM
        // close funnel. Intercept while any subprocess is running and confirm
        // first; the modal renders next frame via the desktop's show_modals.
        if self.started
            && !self.force_quit
            && ctx.input(|i| i.viewport().close_requested())
            && self.desktop.begin_quit_confirm()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if self.desktop.take_quit_confirmed() {
            self.flush_workspace();
            self.force_quit = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // Closing the last project quits the app — an empty desktop is a dead
        // end, and terminal emulators (tmux, Windows Terminal) exit with their
        // last session. `deserted` stays false while the dir picker or the
        // settings modal is up, so a project being created mid-modal survives.
        if self.started && !self.landing_enabled && self.desktop.deserted() {
            self.flush_workspace();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // Capture any zoom a pane applied this frame (Ctrl+Scroll / Ctrl+0) and
        // panel collapse/width, persist after a debounce so a scroll/drag
        // gesture writes the file once.
        let live = terminal::font_size(&ctx);
        let mut settings_dirty = false;
        if live != self.settings.font_size {
            self.settings.font_size = live;
            settings_dirty = true;
        }
        if let Some((collapsed, width)) = self.desktop.panel_prefs() {
            if collapsed != self.settings.panel_collapsed
                || (width - self.settings.panel_width).abs() > 0.5
            {
                self.settings.panel_collapsed = collapsed;
                self.settings.panel_width = width;
                settings_dirty = true;
            }
        }
        if settings_dirty {
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
        // Workspace layout: poll structural dirty, debounce write to workspace.json.
        if self.desktop.poll_workspace_dirty() {
            self.workspace_dirty_at = Some(std::time::Instant::now());
        }
        if let Some(t) = self.workspace_dirty_at {
            if t.elapsed() >= WORKSPACE_SAVE_DEBOUNCE {
                self.flush_workspace();
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

        // Transient toasts, on top of everything (chrome included).
        self.notify.show(&ctx, std::time::Instant::now());

        // Adaptive repaint cadence. The real fast paths are all event-driven and
        // immediate (~0.2ms): reader threads request_repaint() on every PTY chunk,
        // serve() does the same on every dispatch, and winit wakes us on input.
        // The timer below is only an idle backstop. Windows' ~15.6ms default timer
        // granularity floors any request_repaint_after under it, so a tight value
        // here just means "as soon as the OS allows"; we stay hot for a short tail
        // after activity, then idle slowly to avoid pinning 60fps across many
        // terminals.
        let pty = terminal::take_pty_output();
        let input = ctx.input(|i| !i.events.is_empty());
        if pty || input || ctrl_activity {
            self.last_activity = Some(std::time::Instant::now());
        }
        let hot = self
            .last_activity
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(250));
        let cadence = if hot { 4 } else { 100 };
        ctx.request_repaint_after(std::time::Duration::from_millis(cadence));

        // Record deliberate project opens (landing, leader picker) into recents.
        // CLI `foreman open` never creates projects, so it never appears here.
        for (path, cmd) in self.desktop.take_opened() {
            self.recents
                .record(path, recents::kind_of_command(cmd.as_deref()));
        }
    }

    fn on_exit(&mut self) {
        self.flush_workspace();
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

/// Decode an embedded app-icon PNG to unpremultiplied RGBA. Separate from the
/// terminal graphics decoder: no MAX_PIXELS gate, soft-fail at launch only.
fn decode_app_png(data: &[u8]) -> Result<(u32, u32, Vec<u8>), &'static str> {
    let mut dec = png::Decoder::new(data);
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().map_err(|_| "bad png")?;
    let (w, h) = {
        let info = reader.info();
        (info.width, info.height)
    };
    if w == 0 || h == 0 {
        return Err("empty png");
    }
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|_| "bad png")?;
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
        _ => Err("bad png"),
    }
}

/// Center `rgba` (w×h) on a transparent square whose side is a multiple of 4
/// (egui IconData guidance). Does not stretch.
fn pad_to_square_rgba(w: u32, h: u32, rgba: &[u8]) -> (u32, u32, Vec<u8>) {
    let side = w.max(h).div_ceil(4) * 4;
    let mut out = vec![0u8; (side as usize) * (side as usize) * 4];
    let ox = ((side - w) / 2) as usize;
    let oy = ((side - h) / 2) as usize;
    let src_stride = (w as usize) * 4;
    let dst_stride = (side as usize) * 4;
    for row in 0..(h as usize) {
        let src = row * src_stride;
        let dst = (oy + row) * dst_stride + ox * 4;
        out[dst..dst + src_stride].copy_from_slice(&rgba[src..src + src_stride]);
    }
    (side, side, out)
}

/// Taskbar / Alt-Tab icon from the embedded PNG. Soft-fails (None) so a bad
/// asset never blocks launch.
fn load_app_icon() -> Option<egui::IconData> {
    const BYTES: &[u8] = include_bytes!("../assets/icons/app-icon.png");
    let (w, h, rgba) = match decode_app_png(BYTES) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("foreman: app icon decode failed: {e}");
            return None;
        }
    };
    if rgba.len() != (w as usize) * (h as usize) * 4 {
        eprintln!("foreman: app icon size mismatch");
        return None;
    }
    let (sw, sh, square) = pad_to_square_rgba(w, h, &rgba);
    Some(egui::IconData {
        rgba: square,
        width: sw,
        height: sh,
    })
}

// ---- Font fallbacks (CJK / emoji) ------------------------------------------
// egui defaults cover Latin well and almost no CJK. Grid/selection already
// handle wide chars; this only supplies glyphs so those cells don't draw as
// empty boxes (tofu). Fallbacks append after primaries — never replace them.
// See docs/font-fallback.md.

/// Append named font blobs as lowest-priority fallbacks for Monospace and
/// Proportional. Empty blobs are skipped. Existing primary fonts stay first.
/// Pure: no filesystem, no Context — unit-tested with fake bytes.
fn append_font_fallbacks(
    fonts: &mut egui::FontDefinitions,
    named_fonts: impl IntoIterator<Item = (String, Vec<u8>)>,
) {
    for (name, bytes) in named_fonts {
        if bytes.is_empty() {
            continue;
        }
        fonts.font_data.insert(
            name.clone(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
            if let Some(list) = fonts.families.get_mut(&family) {
                if !list.iter().any(|n| n == &name) {
                    list.push(name.clone());
                }
            }
        }
    }
}

/// Known Windows system fonts used as glyph fallbacks (CJK + emoji shapes).
/// Order: CJK first, emoji second (both lowest priority after defaults).
fn windows_fallback_font_paths() -> &'static [(&'static str, &'static str)] {
    &[
        ("yahei", r"C:\Windows\Fonts\msyh.ttc"),
        ("seguiemj", r"C:\Windows\Fonts\seguiemj.ttf"),
    ]
}

/// Build default FontDefinitions plus any fallbacks `read` can supply.
/// Inject `read` so tests never touch the real disk.
fn load_font_definitions(
    read: &dyn Fn(&std::path::Path) -> std::io::Result<Vec<u8>>,
) -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    let mut loaded = Vec::new();
    for &(name, path) in windows_fallback_font_paths() {
        match read(std::path::Path::new(path)) {
            Ok(bytes) if !bytes.is_empty() => loaded.push((name.to_string(), bytes)),
            Ok(_) => {}  // empty file — skip
            Err(_) => {} // missing / unreadable — skip
        }
    }
    append_font_fallbacks(&mut fonts, loaded);
    fonts
}

fn main() -> eframe::Result {
    // Subcommand = thin pipe client (`foreman open ...`), no GUI.
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        std::process::exit(control::client_main(&args[1..]));
    }
    install_panic_logger();
    skills_install::install();
    conpty_install::ensure_conpty().map_err(|e| eframe::Error::AppCreation(Box::new(e)))?;
    let (tx, rx) = std::sync::mpsc::channel();
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 800.0])
        .with_decorations(false);
    match load_app_icon() {
        Some(icon) => viewport = viewport.with_icon(icon),
        None => eprintln!("foreman: using default window icon"),
    }
    let opts = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Foreman",
        opts,
        Box::new(move |cc| {
            // CJK/emoji glyph coverage (tofu fix). Best-effort; missing system
            // fonts leave defaults alone. Once per process — not per frame.
            cc.egui_ctx
                .set_fonts(load_font_definitions(&|p| std::fs::read(p)));

            // Spawn the control server here (not before run_native) so it can hold
            // the egui Context and wake the render loop the instant a dispatch
            // arrives, rather than waiting on the idle repaint tick.
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || control::serve(control::PIPE, tx, ctx));
            Ok(Box::new(App::new(rx)))
        }),
    )
}

#[cfg(test)]
mod app_icon_tests {
    use super::*;

    #[test]
    fn pad_to_square_centers_and_rounds_side_to_multiple_of_4() {
        // 3×5 red pixel field → side max(3,5)=5 → round up to 8.
        let mut src = vec![0u8; 3 * 5 * 4];
        for px in src.chunks_exact_mut(4) {
            px.copy_from_slice(&[255, 0, 0, 255]);
        }
        let (sw, sh, out) = pad_to_square_rgba(3, 5, &src);
        assert_eq!((sw, sh), (8, 8));
        assert_eq!(out.len(), 8 * 8 * 4);
        // Center offset: ((8-3)/2, (8-5)/2) = (2, 1).
        let at = |x: usize, y: usize| {
            let i = (y * 8 + x) * 4;
            [out[i], out[i + 1], out[i + 2], out[i + 3]]
        };
        assert_eq!(at(2, 1), [255, 0, 0, 255]); // top-left of blit
        assert_eq!(at(4, 5), [255, 0, 0, 255]); // bottom-right of blit
        assert_eq!(at(0, 0), [0, 0, 0, 0]); // padding
        assert_eq!(at(7, 7), [0, 0, 0, 0]);
    }

    #[test]
    fn embedded_app_icon_loads_as_square_icon_data() {
        let icon = load_app_icon().expect("app-icon.png should decode");
        assert_eq!(icon.width, icon.height);
        assert_eq!(icon.width % 4, 0);
        assert_eq!(
            icon.rgba.len(),
            (icon.width as usize) * (icon.height as usize) * 4
        );
        // Asset is non-trivial art; must not be an empty transparent square.
        assert!(icon.rgba.iter().any(|&b| b != 0));
    }
}

#[cfg(test)]
mod font_fallback_tests {
    use super::*;

    fn mono_names(fonts: &egui::FontDefinitions) -> Vec<String> {
        fonts
            .families
            .get(&egui::FontFamily::Monospace)
            .cloned()
            .unwrap_or_default()
    }

    fn prop_names(fonts: &egui::FontDefinitions) -> Vec<String> {
        fonts
            .families
            .get(&egui::FontFamily::Proportional)
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn append_fallbacks_pushes_name_to_mono_and_proportional() {
        let mut fonts = egui::FontDefinitions::default();
        let before_mono = mono_names(&fonts);
        let before_prop = prop_names(&fonts);

        append_font_fallbacks(&mut fonts, [("yahei".into(), vec![0u8, 1, 2, 3])]);

        let after_mono = mono_names(&fonts);
        let after_prop = prop_names(&fonts);
        assert_eq!(&after_mono[..before_mono.len()], &before_mono[..]);
        assert_eq!(after_mono.last().map(String::as_str), Some("yahei"));
        assert_eq!(&after_prop[..before_prop.len()], &before_prop[..]);
        assert_eq!(after_prop.last().map(String::as_str), Some("yahei"));
        assert!(fonts.font_data.contains_key("yahei"));
    }

    #[test]
    fn append_fallbacks_preserves_primary_first() {
        // First entry stays primary — tofu fix must not replace Hack/etc.
        let mut fonts = egui::FontDefinitions::default();
        let primary = mono_names(&fonts)
            .first()
            .cloned()
            .expect("default mono family non-empty");

        append_font_fallbacks(&mut fonts, [("seguiemj".into(), vec![9u8, 9, 9])]);

        assert_eq!(
            mono_names(&fonts).first().map(String::as_str),
            Some(primary.as_str())
        );
        assert_eq!(
            mono_names(&fonts).last().map(String::as_str),
            Some("seguiemj")
        );
    }

    #[test]
    fn append_fallbacks_skips_empty_blob() {
        let mut fonts = egui::FontDefinitions::default();
        let before = mono_names(&fonts);

        append_font_fallbacks(&mut fonts, [("empty".into(), Vec::new())]);

        assert_eq!(mono_names(&fonts), before);
        assert!(!fonts.font_data.contains_key("empty"));
    }

    #[test]
    fn append_fallbacks_two_fonts_order_stable() {
        let mut fonts = egui::FontDefinitions::default();
        append_font_fallbacks(
            &mut fonts,
            [("yahei".into(), vec![1u8]), ("seguiemj".into(), vec![2u8])],
        );
        let mono = mono_names(&fonts);
        let n = mono.len();
        assert!(n >= 2);
        assert_eq!(mono[n - 2], "yahei");
        assert_eq!(mono[n - 1], "seguiemj");
    }

    #[test]
    fn append_fallbacks_does_not_duplicate_name_in_family() {
        let mut fonts = egui::FontDefinitions::default();
        append_font_fallbacks(&mut fonts, [("yahei".into(), vec![1u8])]);
        append_font_fallbacks(&mut fonts, [("yahei".into(), vec![2u8])]);
        let count = mono_names(&fonts)
            .iter()
            .filter(|n| n.as_str() == "yahei")
            .count();
        assert_eq!(count, 1);
        assert!(fonts.font_data.contains_key("yahei"));
    }

    #[test]
    fn windows_fallback_paths_name_yahei_and_seguiemj() {
        let paths = windows_fallback_font_paths();
        let names: Vec<&str> = paths.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"yahei"));
        assert!(names.contains(&"seguiemj"));
        for (_, p) in paths {
            assert!(p.starts_with(r"C:\Windows\Fonts\"), "{p}");
        }
    }

    #[test]
    fn load_font_definitions_skips_missing_files() {
        let fonts = load_font_definitions(&|_| {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "nope"))
        });
        assert!(!fonts.font_data.contains_key("yahei"));
        assert!(!fonts.font_data.contains_key("seguiemj"));
        assert!(
            fonts
                .families
                .get(&egui::FontFamily::Monospace)
                .map(|v| !v.is_empty())
                .unwrap_or(false)
        );
    }

    #[test]
    fn load_font_definitions_installs_readable_fonts() {
        let fonts = load_font_definitions(&|path| {
            let s = path.to_string_lossy();
            if s.contains("msyh") {
                Ok(vec![0xAA])
            } else if s.contains("seguiemj") {
                Ok(vec![0xBB])
            } else {
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "x"))
            }
        });
        assert!(fonts.font_data.contains_key("yahei"));
        assert!(fonts.font_data.contains_key("seguiemj"));
        let mono = fonts
            .families
            .get(&egui::FontFamily::Monospace)
            .cloned()
            .unwrap();
        assert_eq!(mono.last().map(String::as_str), Some("seguiemj"));
        assert!(mono.iter().any(|n| n == "yahei"));
    }
}
