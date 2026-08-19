// Release builds are GUI-subsystem so launching foreman.exe from Explorer
// does not spawn a console window. Debug builds stay console-subsystem so
// eprintln/panic output lands somewhere during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appearance;
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
mod search;
mod settings;
mod settings_menu;
mod skills_install;
mod terminal;
mod terminal_font;
mod theme;
mod update;
mod wm;
mod workspace;

use eframe::egui;
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
    /// Update-check events from the worker (update::spawn); drained per-frame.
    update_rx: std::sync::mpsc::Receiver<update::Event>,
    /// Effects for the worker to execute (fetch now / open releases page).
    update_fx: std::sync::mpsc::Sender<update::Effect>,
    /// Current updater state; rendered by the panel chip (a later task).
    update_state: update::State,
    /// Persisted app settings. Seeded into egui's per-context data each frame and
    /// read back to capture edits from any channel: Ctrl+Scroll/Ctrl+0 zoom, panel
    /// collapse/dock, and the settings menu.
    settings: config::Settings,
    /// Set when the live settings diverged from `settings` (font zoom, panel prefs,
    /// or a settings-menu edit); the change is written to disk only after a short
    /// debounce so a whole scroll gesture or a burst of edits persists once.
    font_dirty_at: Option<std::time::Instant>,
    /// Set when the desktop workspace layout changed; written after debounce.
    workspace_dirty_at: Option<std::time::Instant>,
    /// The active color theme, resolved from `settings.theme` (the name) and
    /// seeded into ctx data each frame like `settings`. The Appearance pane edits
    /// it live through the ctx seam; the App reads the edit back here.
    active_theme: std::sync::Arc<crate::theme::Theme>,
    /// The name `active_theme` was resolved from. When `settings.theme` diverges
    /// (a preset switch / Duplicate), the App reloads `active_theme` from the new
    /// name — this tracks the last-loaded name so a reload fires exactly once.
    active_theme_name: String,
    /// Set when a live theme edit diverged from `active_theme`; the edit is
    /// written to the user theme file only after a debounce (mirrors
    /// `font_dirty_at`). The built-in is never written.
    theme_dirty_at: Option<std::time::Instant>,
    /// Last time anything happened (input, PTY output, control msg). Drives the
    /// adaptive repaint cadence: fast while recently active, slow when idle.
    last_activity: Option<std::time::Instant>,
    /// Set once the quit confirm was accepted, so the next viewport Close isn't
    /// intercepted again.
    force_quit: bool,
    /// The empty-state landing screen (wordmark + inline picker + session
    /// icons), shown when no project is visible (`should_show_landing`) and
    /// `landing_enabled` — including when every project is merely minimized.
    landing: landing::Landing,
    /// Default-on: an empty *visible* desktop shows the landing beside the
    /// Sessions panel. `FOREMAN_NO_LANDING=1` restores the old behavior
    /// (startup auto-opens a project in cwd, closing the last project quits).
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

/// Wait this long after the last settings change (zoom, panel prefs, or a menu
/// edit) before writing `settings.json`.
const FONT_SAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);
/// Wait this long after the last structural workspace change before writing
/// `workspace.json` (slightly longer than font — capture walks the full tree).
const WORKSPACE_SAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(600);

impl App {
    fn new(
        ctrl: std::sync::mpsc::Receiver<control::CtrlMsg>,
        update_rx: std::sync::mpsc::Receiver<update::Event>,
        update_fx: std::sync::mpsc::Sender<update::Effect>,
    ) -> Self {
        // Debug-only preview: FOREMAN_UPDATE_TEST=1 fakes an available update
        // so the chip can be seen/screenshotted without a real newer release.
        let update_state =
            if cfg!(debug_assertions) && std::env::var_os("FOREMAN_UPDATE_TEST").is_some() {
                update::State::UpdateAvailable {
                    version: "v9.9.9".into(),
                    html_url: update::RELEASES_URL.into(),
                    can_apply: false,
                }
            } else {
                update::State::Idle
            };
        // Resolve the active theme from the persisted setting up front (the name
        // may reference a user theme file); both feed the struct literal below.
        let settings = config::Settings::load();
        let active_theme = std::sync::Arc::new(crate::theme::Theme::load(&settings.theme));
        let active_theme_name = settings.theme.clone();
        Self {
            desktop: WindowManager::new().as_desktop(),
            started: false,
            chrome_open: false,
            chrome_enter_since: None,
            chrome_leave_since: None,
            chrome_t: 0.0,
            ctrl,
            update_rx,
            update_fx,
            update_state,
            settings,
            font_dirty_at: None,
            workspace_dirty_at: None,
            active_theme,
            active_theme_name,
            theme_dirty_at: None,
            last_activity: None,
            force_quit: false,
            landing: landing::Landing::new(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            ),
            landing_enabled: std::env::var_os("FOREMAN_NO_LANDING").is_none(),
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
        let th = crate::theme::live(ctx);
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
                egui::Stroke::new(APP_BORDER_W, th.app_border()),
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
                p.rect_filled(bar, 0.0, th.chrome_bg);
                p.line_segment(
                    [bar.left_bottom(), bar.right_bottom()],
                    egui::Stroke::new(1.0, th.chrome_border),
                );
                p.text(
                    egui::pos2(bar.min.x + 12.0, bar.center().y),
                    egui::Align2::LEFT_CENTER,
                    "Foreman",
                    egui::FontId::proportional(13.0),
                    th.dim,
                );

                for (resp, glyph) in [
                    (&minb, Glyph::Min),
                    (&maxb, Glyph::Max),
                    (&close, Glyph::Close),
                ] {
                    let hovered = resp.hovered();
                    let mut bg = th.chrome_bg;
                    if hovered {
                        bg = if glyph == Glyph::Close {
                            th.chrome_close_hover
                        } else {
                            th.chrome_btn_hover
                        };
                        p.rect_filled(resp.rect, 0.0, bg);
                    }
                    let col = if hovered { th.text } else { th.dim };
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
            // unless the landing screen owns the empty desktop. Gated on the
            // Startup pane's "Restore workspace on launch" toggle — when it's
            // off, `restored` stays false so the fallback below still runs.
            let mut restored = false;
            if self.settings.restore_workspace {
                let snap = workspace::WorkspaceSnapshot::load();
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
            }
            // Task-manager panel is always present (docked leaf); not in the
            // workspace snapshot — prefs come from settings.json.
            self.desktop.ensure_panel(
                self.settings.panel_collapsed,
                self.settings.panel_width,
                self.settings.panel_dock,
            );
            if !restored && !self.landing_enabled {
                let dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                // self.settings, not config::live: this startup block runs
                // before the frame's seed_live, where live() is still default.
                let nid =
                    self.desktop
                        .add_project(self.settings.default_shell.to_shell(), dir, &ctx);
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

        while let Ok(ev) = self.update_rx.try_recv() {
            let state = std::mem::replace(&mut self.update_state, update::State::Idle);
            let (state, effects) = update::step(state, ev, env!("CARGO_PKG_VERSION"));
            self.update_state = state;
            for fx in effects {
                let _ = self.update_fx.send(fx);
            }
        }

        self.desktop.set_update_chip(match &self.update_state {
            update::State::UpdateAvailable { version, .. } => Some(version.clone()),
            _ => None,
        });

        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let mut area = ui.available_rect_before_wrap();
        if !maximized {
            // The painted frame strokes the outer APP_BORDER_W inside the
            // window edge; keep the desktop inside it, not under it.
            area = area.shrink(APP_BORDER_W);
        }
        // Make the persisted font size the live value every pane reads this frame.
        terminal::set_font_size(&ctx, self.settings.font_size);
        terminal::set_bell_enabled(&ctx, self.settings.bell);
        self.notify
            .set_ttl(std::time::Duration::from_secs_f32(self.settings.toast_secs));
        // Publish the whole settings struct into ctx data so the settings menu
        // (in wm) can read + edit it this frame; its edits come back via config::live.
        config::seed_live(&ctx, &self.settings);
        crate::theme::seed_live(&ctx, &self.active_theme);
        // Install the active theme as egui's widget palette so egui-native controls
        // (the settings menu's combo/text/buttons/color pickers/scrollbar, the
        // close-confirm modal) match the hand-painted rest of the app instead of
        // egui's stock grey dark theme. Cheap per-frame; the chrome tracks a live
        // color edit one frame behind, the same lag every terminal repaint already has.
        ctx.set_visuals(self.active_theme.visuals());
        // Landing when nothing is *visible* (closed or all minimized). Always
        // still run the desktop so the Sessions panel stays docked at its
        // remembered size and minimized PTYs keep pumping; the landing paints
        // in the content rect beside/above that strip.
        let show_landing = self.landing_enabled && self.desktop.should_show_landing();
        if show_landing {
            if !self.landing_shown {
                self.landing.reopen(); // re-focus the field each time we land here
                self.landing_shown = true;
            }
        } else {
            self.landing_shown = false;
        }
        // Skip the desktop only when quitting on a truly empty desktop (no
        // landing gate) — otherwise the panel / minimized keepalive must run.
        let quitting_empty = self.started && !self.landing_enabled && self.desktop.deserted();
        if !quitting_empty {
            self.desktop
                .show(ui, area, true, egui::Id::new("desktop"), false);
        }
        if self.desktop.take_update_click() {
            let state = std::mem::replace(&mut self.update_state, update::State::Idle);
            let (state, effects) =
                update::step(state, update::Event::ClickChip, env!("CARGO_PKG_VERSION"));
            self.update_state = state;
            for fx in effects {
                let _ = self.update_fx.send(fx);
            }
        }
        // "Check for updates now" (Startup pane): fires the same fetch the
        // launch check and periodic re-check use, regardless of `update_check`
        // or the state machine's current state.
        if self.desktop.take_check_updates_requested() {
            let _ = self.update_fx.send(update::Effect::FetchLatest);
        }
        if show_landing {
            let content = self.desktop.landing_content_rect(area);
            if let Some(act) = self.landing.show(ui, content, self.recents.entries()) {
                match act.kind.launch_command() {
                    // Terminal: a plain shell, as before.
                    None => {
                        let nid = self.desktop.add_project(
                            self.settings.default_shell.to_shell(),
                            act.path,
                            &ctx,
                        );
                        self.desktop.tile_new(nid, None);
                    }
                    // Agent (Claude/Codex/Grok), installed: a normal shell that runs it.
                    Some(cmd) if act.kind.installed() => {
                        let nid = self.desktop.add_project_with_command(act.path, cmd, &ctx);
                        self.desktop.tile_new(nid, None);
                    }
                    // Agent missing: an error toast; stay on the landing.
                    Some(_) => self.notify.push(
                        notify::Level::Error,
                        format!("{} isn't installed", act.kind.label()),
                        std::time::Instant::now(),
                    ),
                }
            }
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
        let mut settings_dirty = false;
        // Adopt any settings-menu edits published this frame (config::seed_live in
        // wm). Do this first, then let the font-zoom and panel channels override
        // their own fields below — the menu never touches those, so live_cfg's
        // copies of them equal this frame's seed and won't stomp a live zoom.
        let live_cfg = config::live(&ctx);
        if *live_cfg != self.settings {
            self.settings = (*live_cfg).clone();
            settings_dirty = true;
        }
        let live = terminal::font_size(&ctx);
        if live != self.settings.font_size {
            self.settings.font_size = live;
            settings_dirty = true;
        }
        if let Some((collapsed, width, dock)) = self.desktop.panel_prefs() {
            if collapsed != self.settings.panel_collapsed
                || (width - self.settings.panel_width).abs() > 0.5
                || dock != self.settings.panel_dock
            {
                self.settings.panel_collapsed = collapsed;
                self.settings.panel_width = width;
                self.settings.panel_dock = dock;
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
        // Reload the theme when the active name changes (a preset switch or
        // Duplicate updated settings.theme). Do this before the live read-back and
        // re-seed the ctx so the read-back sees the reloaded theme (equal → not
        // marked dirty — a reload must not masquerade as an edit to persist).
        if self.settings.theme != self.active_theme_name {
            // A name change is a preset switch / rename / duplicate — the settings
            // menu already persisted the outgoing edit to the right file BEFORE
            // changing the name, so just drop the pending flag and load the new one
            // (no flush here, which would otherwise re-create a just-renamed file).
            self.theme_dirty_at = None;
            self.active_theme =
                std::sync::Arc::new(crate::theme::Theme::load(&self.settings.theme));
            self.active_theme_name = self.settings.theme.clone();
            crate::theme::seed_live(&ctx, &self.active_theme);
        }
        // Adopt any Appearance-pane theme edit published this frame (theme::seed_live
        // in wm) so the preview and every real terminal repaint live, and mark it
        // dirty for the debounced write below.
        let live_th = crate::theme::live(&ctx);
        if *live_th != *self.active_theme {
            self.active_theme = live_th;
            self.theme_dirty_at = Some(std::time::Instant::now());
        }
        // Debounced persistence to the user theme file (mirrors the settings
        // debounce). The built-in is never written — editing it live-applies but
        // Duplicate is what creates a user theme.
        if let Some(t) = self.theme_dirty_at {
            if t.elapsed() >= FONT_SAVE_DEBOUNCE {
                if !crate::theme::Theme::is_builtin(&self.settings.theme) {
                    if let Err(e) = self.active_theme.save(&self.settings.theme) {
                        eprintln!("foreman: could not save theme: {e}");
                    }
                }
                self.theme_dirty_at = None;
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

/// GUI-subsystem processes start with no console, so the CLI path must adopt
/// the parent shell's console before printing or `foreman status` etc. would
/// write into the void. Std handles the caller already redirected (pipes,
/// files) are valid and left untouched; only null handles are bound to the
/// attached console. No-op when a console is already attached (debug builds)
/// or when there is no parent console (double-click launch).
fn attach_parent_console() {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE, SetStdHandle,
    };
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        let bind = |slot: u32, name: &[u16], access: u32| {
            let cur = GetStdHandle(slot);
            if !cur.is_null() && cur != INVALID_HANDLE_VALUE {
                return; // caller-redirected pipe/file: keep it
            }
            let h = CreateFileW(
                name.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if h != INVALID_HANDLE_VALUE {
                SetStdHandle(slot, h);
            }
        };
        // "CONOUT$" / "CONIN$" as UTF-16, NUL-terminated.
        let conout: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        let conin: Vec<u16> = "CONIN$\0".encode_utf16().collect();
        bind(STD_OUTPUT_HANDLE, &conout, GENERIC_WRITE);
        bind(STD_ERROR_HANDLE, &conout, GENERIC_WRITE);
        bind(STD_INPUT_HANDLE, &conin, GENERIC_READ);
    }
}

fn main() -> eframe::Result {
    // Subcommand = thin pipe client (`foreman open ...`), no GUI.
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        attach_parent_console();
        std::process::exit(control::client_main(&args[1..]));
    }
    install_panic_logger();
    // Gate on `Settings::install_skills` / `Settings::update_check` — both take
    // effect next launch (their menu rows already say "on launch"). Loaded once
    // here, ahead of the App's own `Settings::load()`, since this runs before
    // any frame exists.
    let startup_settings = config::Settings::load();
    if startup_settings.install_skills {
        skills_install::install();
    }
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
            // Terminal Hack faces + CJK/emoji fallbacks. Best-effort system
            // fonts; missing files leave defaults alone. Once per process.
            cc.egui_ctx
                .set_fonts(terminal_font::load_font_definitions(&|p| std::fs::read(p)));
            // The app is always the dark warm theme; pin it so a system light-mode
            // preference can't swap in egui's unstyled light visuals. The per-frame
            // `set_visuals` in `App::ui` then paints the active theme over it.
            cc.egui_ctx.set_theme(egui::ThemePreference::Dark);

            // Spawn the control server here (not before run_native) so it can hold
            // the egui Context and wake the render loop the instant a dispatch
            // arrives, rather than waiting on the idle repaint tick.
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || control::serve(control::PIPE, tx, ctx));
            let (upd_event_tx, upd_event_rx) = std::sync::mpsc::channel();
            let (upd_effect_tx, upd_effect_rx) = std::sync::mpsc::channel();
            // Release builds only; FOREMAN_NO_UPDATE=1 is the escape hatch
            // (spec section 3 gating). Debug builds never phone home. The
            // Startup pane's "Check for updates on launch" toggle gates the
            // launch check itself — "Check now" in the menu bypasses this by
            // sending an Effect straight to the App, which works even when the
            // worker was never spawned (the send just no-ops).
            if !cfg!(debug_assertions)
                && std::env::var_os("FOREMAN_NO_UPDATE").is_none()
                && startup_settings.update_check
            {
                update::spawn(cc.egui_ctx.clone(), upd_event_tx, upd_effect_rx);
            }
            Ok(Box::new(App::new(rx, upd_event_rx, upd_effect_tx)))
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
