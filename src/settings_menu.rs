//! Settings menu (phase 1): pure model half. Categories, declarative row
//! descriptors, and a pure `adjust` over &mut Settings — all unit-tested
//! without a GUI. The egui view lives in the same file below (Task 3).

use crate::config::{DefaultShell, Settings};
use crate::theme::*;
use eframe::egui;

/// Left-rail categories, in display order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // constructed/matched by the view (Task 3)
pub enum Pane {
    Terminal,
    Bell,
    WindowManager,
    Keybindings,
    Agents,
    Startup,
}

#[allow(dead_code)] // used by the view (Task 3)
impl Pane {
    pub const ALL: [Pane; 6] = [
        Pane::Terminal,
        Pane::Bell,
        Pane::WindowManager,
        Pane::Keybindings,
        Pane::Agents,
        Pane::Startup,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Pane::Terminal => "Terminal",
            Pane::Bell => "Bell & Alerts",
            Pane::WindowManager => "Window Manager",
            Pane::Keybindings => "Keybindings",
            Pane::Agents => "Agents",
            Pane::Startup => "Startup & Updates",
        }
    }
}

/// One editable setting. Task 3+ match on these exact variants.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // constructed by rows()/adjust(); consumed by the view (Task 3)
pub enum Field {
    DefaultShellF,
    ScrollbackLines,
    ScrollSpeed,
    ZoomStep,
    CopyOnSelect,
    PasteWarn,
    BellOn,
    BellPeriod,
    ToastSecs,
    NewWindowsFloat,
    FocusFollowsMouse,
    DimUnfocused,
    InstallSkills,
    CrewStale,
    SendSettle,
    RestoreWorkspace,
    DefaultProjectDir,
    UpdateCheck,
    OpenKeybindings,
    CheckUpdatesNow,
    OpenConfigFolder,
}

/// How a row's value is edited. Drives the view's widget choice (Task 3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // read by the view (Task 3)
pub enum Kind {
    Toggle,
    Stepper,
    Choice,
    Text,
    Action,
}

/// A declarative row: what field it edits, its label/description, and how
/// it should be presented. `rows()` returns static slices of these per pane.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // fields read by the view (Task 3)
pub struct RowSpec {
    pub field: Field,
    pub label: &'static str,
    pub desc: &'static str,
    pub kind: Kind,
}

/// The static row list for a pane, in display order.
#[allow(dead_code)] // called by the view (Task 3)
pub fn rows(pane: Pane) -> &'static [RowSpec] {
    match pane {
        Pane::Terminal => &[
            RowSpec {
                field: Field::DefaultShellF,
                label: "Default shell",
                desc: "What a new pane runs; per-pane chips still override",
                kind: Kind::Choice,
            },
            RowSpec {
                field: Field::ScrollbackLines,
                label: "Scrollback lines",
                desc: "History kept per pane (new terminals)",
                kind: Kind::Stepper,
            },
            RowSpec {
                field: Field::ScrollSpeed,
                label: "Scroll speed",
                desc: "Lines per wheel notch",
                kind: Kind::Stepper,
            },
            RowSpec {
                field: Field::ZoomStep,
                label: "Zoom step",
                desc: "Font points per Ctrl+Scroll notch",
                kind: Kind::Stepper,
            },
            RowSpec {
                field: Field::CopyOnSelect,
                label: "Copy on select",
                desc: "Selection lands on the clipboard immediately",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::PasteWarn,
                label: "Warn on multi-line paste",
                desc: "Confirm before pasting text containing newlines",
                kind: Kind::Toggle,
            },
        ],
        Pane::Bell => &[
            RowSpec {
                field: Field::BellOn,
                label: "Bell attention",
                desc: "Master switch — pulse (and any future sound) honors it",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::BellPeriod,
                label: "Pulse speed",
                desc: "One full breathe of the amber pulse",
                kind: Kind::Stepper,
            },
            RowSpec {
                field: Field::ToastSecs,
                label: "Toast duration",
                desc: "How long notifications linger top-right",
                kind: Kind::Stepper,
            },
        ],
        Pane::WindowManager => &[
            RowSpec {
                field: Field::NewWindowsFloat,
                label: "New terminals open floating",
                desc: "Off = new terminals join the tiling tree",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::FocusFollowsMouse,
                label: "Focus follows mouse",
                desc: "Hovering a pane focuses it without a click",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::DimUnfocused,
                label: "Dim unfocused panes",
                desc: "Slight darkening on everything but the focused terminal",
                kind: Kind::Toggle,
            },
        ],
        Pane::Keybindings => &[RowSpec {
            field: Field::OpenKeybindings,
            label: "Edit keybindings…",
            desc: "Leader, chords, conflicts — the full editor",
            kind: Kind::Action,
        }],
        Pane::Agents => &[
            RowSpec {
                field: Field::InstallSkills,
                label: "Install agent skills on launch",
                desc: "Writes foreman-dispatch / foreman-chat into Claude & Codex skill dirs",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::CrewStale,
                label: "Crew stale after",
                desc: "A member unheard this long shows its age in amber",
                kind: Kind::Stepper,
            },
            RowSpec {
                field: Field::SendSettle,
                label: "Send settle default",
                desc: "Quiescence wait for foreman send when the caller doesn't pass one",
                kind: Kind::Stepper,
            },
        ],
        Pane::Startup => &[
            RowSpec {
                field: Field::RestoreWorkspace,
                label: "Restore workspace on launch",
                desc: "Reopen last session's projects and layout",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::DefaultProjectDir,
                label: "Default project directory",
                desc: "Where the picker starts browsing (blank = home)",
                kind: Kind::Text,
            },
            RowSpec {
                field: Field::UpdateCheck,
                label: "Check for updates on launch",
                desc: "GitHub releases, background check",
                kind: Kind::Toggle,
            },
            RowSpec {
                field: Field::CheckUpdatesNow,
                label: "Check for updates now",
                desc: "",
                kind: Kind::Action,
            },
            RowSpec {
                field: Field::OpenConfigFolder,
                label: "Open settings folder",
                desc: "%APPDATA%\\foreman",
                kind: Kind::Action,
            },
        ],
    }
}

/// Direction a row's value should move. `Toggle` is meaningless for steppers
/// (ignored) and vice versa (flip ignores it) — each `adjust` arm picks what
/// applies to its field's `Kind`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // constructed by the view (Task 3)
pub enum Adjust {
    Toggle,
    Inc,
    Dec,
}

/// Apply one nav action to one field. Returns whether the value actually
/// changed (steppers at a clamp bound report `false`; `Action` fields are
/// handled by the view and always report `false` here).
#[allow(dead_code)] // called by the view (Task 3)
pub fn adjust(field: Field, a: Adjust, s: &mut Settings) -> bool {
    fn step_f32(v: &mut f32, a: Adjust, step: f32, min: f32, max: f32) -> bool {
        let next = match a {
            Adjust::Inc => (*v + step).min(max),
            Adjust::Dec => (*v - step).max(min),
            Adjust::Toggle => *v,
        };
        let changed = (next - *v).abs() > f32::EPSILON;
        *v = next;
        changed
    }
    fn step_u32(v: &mut u32, a: Adjust, step: u32, min: u32, max: u32) -> bool {
        let next = match a {
            Adjust::Inc => v.saturating_add(step).min(max),
            Adjust::Dec => v.saturating_sub(step).max(min),
            Adjust::Toggle => *v,
        };
        let changed = next != *v;
        *v = next;
        changed
    }
    fn step_u64(v: &mut u64, a: Adjust, step: u64, min: u64, max: u64) -> bool {
        let next = match a {
            Adjust::Inc => v.saturating_add(step).min(max),
            Adjust::Dec => v.saturating_sub(step).max(min),
            Adjust::Toggle => *v,
        };
        let changed = next != *v;
        *v = next;
        changed
    }
    fn flip(v: &mut bool) -> bool {
        *v = !*v;
        true
    }
    match field {
        Field::DefaultShellF => {
            let order = [
                DefaultShell::PowerShell,
                DefaultShell::Cmd,
                DefaultShell::Sh,
            ];
            let i = order
                .iter()
                .position(|x| *x == s.default_shell)
                .unwrap_or(0);
            let n = order.len();
            let next = match a {
                Adjust::Inc | Adjust::Toggle => order[(i + 1) % n],
                Adjust::Dec => order[(i + n - 1) % n],
            };
            let changed = next != s.default_shell;
            s.default_shell = next;
            changed
        }
        Field::ScrollbackLines => step_u32(&mut s.scrollback_lines, a, 1000, 100, 1_000_000),
        Field::ScrollSpeed => step_f32(&mut s.scroll_speed, a, 1.0, 1.0, 30.0),
        Field::ZoomStep => step_f32(&mut s.zoom_step, a, 0.25, 0.25, 5.0),
        Field::CopyOnSelect => flip(&mut s.copy_on_select),
        Field::PasteWarn => flip(&mut s.paste_warn_multiline),
        Field::BellOn => flip(&mut s.bell),
        Field::BellPeriod => step_f32(&mut s.bell_period, a, 0.1, 0.4, 5.0),
        Field::ToastSecs => step_f32(&mut s.toast_secs, a, 1.0, 1.0, 30.0),
        Field::NewWindowsFloat => flip(&mut s.new_windows_float),
        Field::FocusFollowsMouse => flip(&mut s.focus_follows_mouse),
        Field::DimUnfocused => flip(&mut s.dim_unfocused),
        Field::InstallSkills => flip(&mut s.install_skills),
        Field::CrewStale => step_u32(&mut s.crew_stale_secs, a, 30, 30, 3600),
        Field::SendSettle => step_u64(&mut s.send_settle_ms, a, 20, 0, 2000),
        Field::RestoreWorkspace => flip(&mut s.restore_workspace),
        Field::DefaultProjectDir => false,
        Field::UpdateCheck => flip(&mut s.update_check),
        Field::OpenKeybindings | Field::CheckUpdatesNow | Field::OpenConfigFolder => false,
    }
}

/// Render a field's current value as the view's row-trailing text.
#[allow(dead_code)] // called by the view (Task 3)
pub fn display(field: Field, s: &Settings) -> String {
    match field {
        Field::DefaultShellF => s.default_shell.label().to_string(),
        Field::ScrollbackLines => format!("{}", s.scrollback_lines),
        Field::ScrollSpeed => format!("{}", s.scroll_speed),
        Field::ZoomStep => format!("{}", s.zoom_step),
        Field::CopyOnSelect => s.copy_on_select.to_string(),
        Field::PasteWarn => s.paste_warn_multiline.to_string(),
        Field::BellOn => s.bell.to_string(),
        Field::BellPeriod => format!("{} s", s.bell_period),
        Field::ToastSecs => format!("{} s", s.toast_secs),
        Field::NewWindowsFloat => s.new_windows_float.to_string(),
        Field::FocusFollowsMouse => s.focus_follows_mouse.to_string(),
        Field::DimUnfocused => s.dim_unfocused.to_string(),
        Field::InstallSkills => s.install_skills.to_string(),
        Field::CrewStale => {
            if s.crew_stale_secs % 60 == 0 {
                format!("{} min", s.crew_stale_secs / 60)
            } else {
                format!("{} s", s.crew_stale_secs)
            }
        }
        Field::SendSettle => format!("{} ms", s.send_settle_ms),
        Field::RestoreWorkspace => s.restore_workspace.to_string(),
        Field::DefaultProjectDir => s.default_project_dir.clone(),
        Field::UpdateCheck => s.update_check.to_string(),
        Field::OpenKeybindings | Field::CheckUpdatesNow | Field::OpenConfigFolder => String::new(),
    }
}

/// Navigation/focus state for the settings menu. Pure — no egui here; the
/// view (Task 3) drives it from key events and reads it back to render.
#[derive(Clone, Debug)]
#[allow(dead_code)] // driven/read by the view (Task 3)
pub struct SettingsMenu {
    pub pane: Pane,
    pub row: usize,
    pub in_rail: bool,
    /// When `Some`, an inline text field is open for the selected `Text` row,
    /// holding the in-progress edit buffer. `None` = browsing.
    pub editing: Option<String>,
    /// A window-lifecycle outcome (Close / OpenKeybindings / CheckUpdatesNow)
    /// produced this frame, stashed for the WM's `drain_settings` to act on
    /// after the render loop (content cannot mutate the WM mid-loop).
    pub pending: Option<MenuOutcome>,
}

#[allow(dead_code)] // driven by the view (Task 3)
impl SettingsMenu {
    pub fn new() -> Self {
        Self {
            pane: Pane::Terminal,
            row: 0,
            in_rail: true,
            editing: None,
            pending: None,
        }
    }

    /// The menu's intrinsic content size (title + body + footer bands), used to
    /// size the floating window when it is first opened.
    pub fn size() -> egui::Vec2 {
        egui::vec2(WIN_W, TITLE_H + BODY_H + FOOTER_H)
    }

    /// Move up one row in the current pane; clamps at 0 (no wrap).
    pub fn nav_up(&mut self) {
        self.row = self.row.saturating_sub(1);
    }

    /// Move down one row in the current pane; clamps at the last row.
    pub fn nav_down(&mut self) {
        let last = rows(self.pane).len().saturating_sub(1);
        self.row = (self.row + 1).min(last);
    }

    /// Flip focus between the pane rail and the row list.
    pub fn nav_tab(&mut self) {
        self.in_rail = !self.in_rail;
    }

    /// Switch to a different pane, resetting the row cursor.
    pub fn select_pane(&mut self, p: Pane) {
        self.pane = p;
        self.row = 0;
    }
}

impl Default for SettingsMenu {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// View (Task 3): an egui modal over the pure model above. Mirrors the
// keybindings editor's overlay scaffolding (dim layer, centered panel, full
// input capture) — see `src/settings.rs`.
// ---------------------------------------------------------------------------

/// Panel geometry (points). Fixed so the layout reads the same on every pane.
const WIN_W: f32 = 660.0;
const RAIL_W: f32 = 190.0;
const TITLE_H: f32 = 38.0;
const BODY_H: f32 = 300.0;
const FOOTER_H: f32 = 30.0;

/// What the settings menu wants the caller (wm) to do after a frame.
#[derive(Clone, Debug)]
pub enum MenuOutcome {
    /// Stay open, nothing to persist.
    Pending,
    /// A setting changed this frame — caller publishes it + arms the save debounce.
    Changed,
    /// Open the keybindings editor on top of the menu.
    OpenKeybindings,
    /// "Check for updates now" was clicked — caller fires the fetch through
    /// `update_fx` (the menu has no path to that channel itself).
    CheckUpdatesNow,
    /// Close the menu.
    Close,
}

/// Merge a newly-produced outcome into the running one, keeping the
/// highest-priority (Close > OpenKeybindings > Changed > Pending). Keyboard and
/// mouse can both fire in one frame; this stops a stray mouse `Changed` from
/// clobbering a keyboard `Close`.
fn bump(cur: &mut MenuOutcome, new: MenuOutcome) {
    fn rank(o: &MenuOutcome) -> u8 {
        match o {
            MenuOutcome::Pending => 0,
            MenuOutcome::Changed => 1,
            MenuOutcome::CheckUpdatesNow => 2,
            MenuOutcome::OpenKeybindings => 3,
            MenuOutcome::Close => 4,
        }
    }
    if rank(&new) > rank(cur) {
        *cur = new;
    }
}

impl SettingsMenu {
    /// Render one frame of the settings menu and report what the caller should
    /// do. `s` is the live settings, mutated in place; a `Changed` outcome means
    /// the caller republishes it and arms the save debounce.
    pub fn show(&mut self, ui: &mut egui::Ui, s: &mut Settings) -> MenuOutcome {
        // Keyboard drives the menu, unless an inline text edit owns input.
        let mut outcome = if self.editing.is_none() {
            self.handle_keys(ui, s)
        } else {
            MenuOutcome::Pending
        };

        // Dim the desktop, then draw a centered panel.
        let screen = ui.ctx().content_rect();
        ui.painter()
            .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(170));

        egui::Window::new("settings_menu")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                egui::Frame::NONE
                    .fill(WIN_BG)
                    .stroke(egui::Stroke::new(1.0, BORDER_FOCUS))
                    .inner_margin(egui::Margin::same(0))
                    .corner_radius(egui::CornerRadius::same(8)),
            )
            .show(ui.ctx(), |ui| {
                ui.set_min_width(WIN_W);
                ui.set_max_width(WIN_W);
                ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                ui.visuals_mut().override_text_color = Some(TEXT);
                // Fixed width (not available_width) so the manual row geometry is
                // stable on the first frame before the Window has sized itself.
                let w = WIN_W;

                // --- title band ---
                let (title, _) =
                    ui.allocate_exact_size(egui::vec2(w, TITLE_H), egui::Sense::hover());
                ui.painter().rect_filled(
                    title,
                    egui::CornerRadius {
                        nw: 8,
                        ne: 8,
                        sw: 0,
                        se: 0,
                    },
                    TITLE_BG_FOCUS,
                );
                ui.painter().text(
                    egui::pos2(title.min.x + 18.0, title.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("Settings — {}", self.pane.label()),
                    egui::FontId::proportional(15.0),
                    TEXT,
                );

                // --- body: rail | pane ---
                let (body, _) = ui.allocate_exact_size(egui::vec2(w, BODY_H), egui::Sense::hover());
                let rail = egui::Rect::from_min_size(body.min, egui::vec2(RAIL_W, BODY_H));
                let pane =
                    egui::Rect::from_min_max(egui::pos2(body.min.x + RAIL_W, body.min.y), body.max);
                self.draw_rail(ui, rail);
                self.draw_pane(ui, pane, s, &mut outcome);

                // --- footer ---
                let (footer, _) =
                    ui.allocate_exact_size(egui::vec2(w, FOOTER_H), egui::Sense::hover());
                ui.painter().line_segment(
                    [footer.left_top(), footer.right_top()],
                    egui::Stroke::new(1.0, BORDER),
                );
                ui.painter().text(
                    egui::pos2(footer.min.x + 18.0, footer.center().y),
                    egui::Align2::LEFT_CENTER,
                    "↑↓ navigate · Tab rail⇄pane · Enter edit · ←→ adjust · Esc close",
                    egui::FontId::proportional(11.5),
                    DIM,
                );
            });

        outcome
    }

    /// Read this frame's navigation/adjust keys and apply them. Returns the
    /// keyboard-driven outcome; mouse handling in the draw can only raise it.
    fn handle_keys(&mut self, ui: &egui::Ui, s: &mut Settings) -> MenuOutcome {
        let (up, down, tab, left, right, enter, esc) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Tab),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });

        if esc {
            return MenuOutcome::Close;
        }
        if tab {
            self.nav_tab();
        }

        if self.in_rail {
            if up {
                self.prev_pane();
            }
            if down {
                self.next_pane();
            }
            // Enter or → dives from the rail into the pane's rows.
            if enter || right {
                self.in_rail = false;
            }
            return MenuOutcome::Pending;
        }

        if up {
            self.nav_up();
        }
        if down {
            self.nav_down();
        }

        let spec = rows(self.pane)[self.row];
        let mut changed = false;
        if left {
            changed |= adjust(spec.field, Adjust::Dec, s);
        }
        if right {
            changed |= adjust(spec.field, Adjust::Inc, s);
        }
        if enter {
            match spec.kind {
                Kind::Toggle => changed |= adjust(spec.field, Adjust::Toggle, s),
                Kind::Stepper | Kind::Choice => changed |= adjust(spec.field, Adjust::Inc, s),
                Kind::Text => self.editing = Some(display(spec.field, s)),
                Kind::Action => return self.do_action(spec.field),
            }
        }
        if changed {
            MenuOutcome::Changed
        } else {
            MenuOutcome::Pending
        }
    }

    fn pane_index(&self) -> usize {
        Pane::ALL.iter().position(|p| *p == self.pane).unwrap_or(0)
    }

    /// Move the rail selection up a pane (clamps; no wrap, matching row nav).
    fn prev_pane(&mut self) {
        let i = self.pane_index().saturating_sub(1);
        self.select_pane(Pane::ALL[i]);
    }

    /// Move the rail selection down a pane (clamps at the last pane).
    fn next_pane(&mut self) {
        let i = (self.pane_index() + 1).min(Pane::ALL.len() - 1);
        self.select_pane(Pane::ALL[i]);
    }

    /// Run an `Action` row.
    fn do_action(&self, field: Field) -> MenuOutcome {
        match field {
            Field::OpenKeybindings => MenuOutcome::OpenKeybindings,
            Field::CheckUpdatesNow => MenuOutcome::CheckUpdatesNow,
            Field::OpenConfigFolder => {
                if let Some(dir) = crate::config::config_dir() {
                    std::process::Command::new("explorer").arg(dir).spawn().ok();
                }
                MenuOutcome::Pending
            }
            _ => MenuOutcome::Pending,
        }
    }

    /// Left rail: one clickable row per pane; the active pane gets a wash and a
    /// bright left edge.
    fn draw_rail(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        ui.painter().line_segment(
            [rect.right_top(), rect.right_bottom()],
            egui::Stroke::new(1.0, BORDER),
        );
        let row_h = 40.0;
        for (i, p) in Pane::ALL.iter().enumerate() {
            let r = egui::Rect::from_min_size(
                egui::pos2(rect.min.x, rect.min.y + i as f32 * row_h),
                egui::vec2(rect.width(), row_h),
            );
            let active = *p == self.pane;
            let resp = ui.interact(r, egui::Id::new(("settings_rail", i)), egui::Sense::click());
            if resp.clicked() {
                self.select_pane(*p);
                self.in_rail = false;
                self.editing = None; // abandon any inline text edit on pane change
            }
            if active {
                ui.painter().rect_filled(r, 0.0, SEL_BG);
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(r.min, egui::vec2(2.0, row_h)),
                    0.0,
                    BORDER_FOCUS,
                );
            }
            let color = if active || resp.hovered() { TEXT } else { DIM };
            ui.painter().text(
                egui::pos2(r.min.x + 16.0, r.center().y),
                egui::Align2::LEFT_CENTER,
                p.label(),
                egui::FontId::proportional(13.5),
                color,
            );
        }
    }

    /// Right pane: one row per `RowSpec` — label + dim description on the left,
    /// the kind-specific control on the right.
    fn draw_pane(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        s: &mut Settings,
        outcome: &mut MenuOutcome,
    ) {
        let specs = rows(self.pane);
        // Safety net: an inline edit is only valid while its own Text row is the
        // selection. If anything moved the selection away (e.g. a mouse click on
        // the rail or another control) drop the edit — otherwise its TextEdit
        // stops being drawn, never loses focus, and the keyboard stays frozen.
        if self.editing.is_some()
            && !(!self.in_rail && self.row < specs.len() && specs[self.row].kind == Kind::Text)
        {
            self.editing = None;
        }
        let row_h = 46.0;
        let pad = 18.0;
        let pane = self.pane;
        let cur_row = self.row;
        let in_rail = self.in_rail;
        for (i, spec) in specs.iter().enumerate() {
            let r = egui::Rect::from_min_size(
                egui::pos2(rect.min.x, rect.min.y + i as f32 * row_h),
                egui::vec2(rect.width(), row_h),
            );
            let selected = !in_rail && i == cur_row;
            if selected {
                ui.painter().rect_filled(
                    r.shrink2(egui::vec2(6.0, 3.0)),
                    egui::CornerRadius::same(4),
                    SEL_BG,
                );
            }
            ui.painter().text(
                egui::pos2(r.min.x + pad, r.min.y + 16.0),
                egui::Align2::LEFT_CENTER,
                spec.label,
                egui::FontId::proportional(13.0),
                TEXT,
            );
            if !spec.desc.is_empty() {
                ui.painter().text(
                    egui::pos2(r.min.x + pad, r.min.y + 32.0),
                    egui::Align2::LEFT_CENTER,
                    spec.desc,
                    egui::FontId::proportional(11.0),
                    DIM,
                );
            }
            let anchor_x = r.max.x - pad;
            let cy = r.center().y;
            self.draw_control(ui, spec, s, anchor_x, cy, r, selected, outcome, pane, i);
        }
        // Static version stamp, Startup pane only — not a RowSpec (nothing to
        // navigate to or edit), so it's painted directly below the last row.
        if pane == Pane::Startup {
            let y = rect.min.y + specs.len() as f32 * row_h + 20.0;
            ui.painter().text(
                egui::pos2(rect.min.x + pad, y),
                egui::Align2::LEFT_CENTER,
                format!("Foreman v{}", env!("CARGO_PKG_VERSION")),
                egui::FontId::proportional(11.0),
                DIM,
            );
        }
    }

    /// Draw the control for one row and route its mouse interaction.
    #[allow(clippy::too_many_arguments)]
    fn draw_control(
        &mut self,
        ui: &mut egui::Ui,
        spec: &RowSpec,
        s: &mut Settings,
        anchor_x: f32,
        cy: f32,
        row_rect: egui::Rect,
        selected: bool,
        outcome: &mut MenuOutcome,
        pane: Pane,
        idx: usize,
    ) {
        let id = egui::Id::new(("settings_ctl", pane.label(), idx));
        match spec.kind {
            Kind::Toggle => {
                let on = display(spec.field, s) == "true";
                let (w, h) = (34.0, 18.0);
                let bx = egui::Rect::from_min_size(
                    egui::pos2(anchor_x - w, cy - h / 2.0),
                    egui::vec2(w, h),
                );
                let resp = ui.interact(bx, id, egui::Sense::click());
                if resp.clicked() && adjust(spec.field, Adjust::Toggle, s) {
                    bump(outcome, MenuOutcome::Changed);
                }
                let col = if on { BELL } else { BORDER };
                ui.painter().rect_stroke(
                    bx,
                    egui::CornerRadius::same(9),
                    egui::Stroke::new(1.5, col),
                    egui::StrokeKind::Inside,
                );
                let knob_r = 6.0;
                let kx = if on {
                    bx.max.x - knob_r - 2.0
                } else {
                    bx.min.x + knob_r + 2.0
                };
                ui.painter()
                    .circle_filled(egui::pos2(kx, cy), knob_r, if on { BELL } else { DIM });
            }
            Kind::Stepper => {
                let val = display(spec.field, s);
                let plus = egui::Rect::from_min_size(
                    egui::pos2(anchor_x - 20.0, cy - 10.0),
                    egui::vec2(20.0, 20.0),
                );
                let vw = 88.0;
                let value_rect = egui::Rect::from_min_size(
                    egui::pos2(plus.min.x - vw, cy - 10.0),
                    egui::vec2(vw, 20.0),
                );
                let minus = egui::Rect::from_min_size(
                    egui::pos2(value_rect.min.x - 20.0, cy - 10.0),
                    egui::vec2(20.0, 20.0),
                );
                let rp = ui.interact(plus, id.with("plus"), egui::Sense::click());
                let rm = ui.interact(minus, id.with("minus"), egui::Sense::click());
                if rp.clicked() && adjust(spec.field, Adjust::Inc, s) {
                    bump(outcome, MenuOutcome::Changed);
                }
                if rm.clicked() && adjust(spec.field, Adjust::Dec, s) {
                    bump(outcome, MenuOutcome::Changed);
                }
                let border = if selected { BORDER_FOCUS } else { BORDER };
                for (rc, sym, hov) in [(minus, "−", rm.hovered()), (plus, "+", rp.hovered())] {
                    ui.painter().rect_stroke(
                        rc,
                        egui::CornerRadius::same(4),
                        egui::Stroke::new(1.0, border),
                        egui::StrokeKind::Inside,
                    );
                    ui.painter().text(
                        rc.center(),
                        egui::Align2::CENTER_CENTER,
                        sym,
                        egui::FontId::proportional(14.0),
                        if hov { TEXT } else { DIM },
                    );
                }
                ui.painter().text(
                    egui::pos2(value_rect.max.x - 4.0, cy),
                    egui::Align2::RIGHT_CENTER,
                    val,
                    egui::FontId::proportional(12.5),
                    TEXT,
                );
            }
            Kind::Choice => {
                let val = display(spec.field, s);
                let galley = ui.painter().layout_no_wrap(
                    val.clone(),
                    egui::FontId::proportional(12.5),
                    TEXT,
                );
                let w = galley.size().x + 24.0;
                let chip = egui::Rect::from_min_size(
                    egui::pos2(anchor_x - w, cy - 11.0),
                    egui::vec2(w, 22.0),
                );
                let resp = ui.interact(chip, id, egui::Sense::click());
                if resp.clicked() && adjust(spec.field, Adjust::Inc, s) {
                    bump(outcome, MenuOutcome::Changed);
                }
                let border = if selected || resp.hovered() {
                    BORDER_FOCUS
                } else {
                    BORDER
                };
                ui.painter().rect_stroke(
                    chip,
                    egui::CornerRadius::same(4),
                    egui::Stroke::new(1.0, border),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    chip.center(),
                    egui::Align2::CENTER_CENTER,
                    val,
                    egui::FontId::proportional(12.5),
                    TEXT,
                );
            }
            Kind::Text => {
                if self.editing.is_some() && selected {
                    let te_w = (row_rect.width() - 220.0).clamp(160.0, 300.0);
                    let te_rect = egui::Rect::from_min_size(
                        egui::pos2(anchor_x - te_w, cy - 12.0),
                        egui::vec2(te_w, 24.0),
                    );
                    let buf = self.editing.as_mut().unwrap();
                    let resp = ui.put(te_rect, egui::TextEdit::singleline(buf).desired_width(te_w));
                    // Canonical egui commit/cancel: Enter loses focus AND is
                    // pressed → commit; any other focus loss (Esc, click-away)
                    // cancels. request_focus keeps the field hot until then.
                    if resp.lost_focus() {
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let committed = self.editing.take().unwrap();
                            // Every Kind::Text field needs a commit arm here —
                            // there is no generic string path through adjust().
                            match spec.field {
                                Field::DefaultProjectDir => {
                                    s.default_project_dir = committed;
                                    bump(outcome, MenuOutcome::Changed);
                                }
                                other => {
                                    debug_assert!(
                                        false,
                                        "Kind::Text field {other:?} has no commit arm"
                                    );
                                }
                            }
                        } else {
                            self.editing = None;
                        }
                    } else {
                        resp.request_focus();
                    }
                } else {
                    let raw = display(spec.field, s);
                    let shown = if raw.is_empty() {
                        "(home)".to_string()
                    } else {
                        raw
                    };
                    let galley = ui.painter().layout_no_wrap(
                        shown.clone(),
                        egui::FontId::proportional(12.5),
                        TEXT,
                    );
                    let w = (galley.size().x + 8.0).max(60.0);
                    let hit = egui::Rect::from_min_size(
                        egui::pos2(anchor_x - w, cy - 12.0),
                        egui::vec2(w, 24.0),
                    );
                    let resp = ui.interact(hit, id, egui::Sense::click());
                    if resp.clicked() {
                        self.editing = Some(display(spec.field, s));
                    }
                    ui.painter().text(
                        egui::pos2(anchor_x, cy),
                        egui::Align2::RIGHT_CENTER,
                        shown,
                        egui::FontId::proportional(12.5),
                        if resp.hovered() { TEXT } else { DIM },
                    );
                }
            }
            Kind::Action => {
                let caption = match spec.field {
                    Field::OpenKeybindings => "Open editor",
                    Field::OpenConfigFolder => "Open folder",
                    Field::CheckUpdatesNow => "Check now",
                    _ => "Open",
                };
                let galley = ui.painter().layout_no_wrap(
                    caption.to_string(),
                    egui::FontId::proportional(12.5),
                    TEXT,
                );
                let w = galley.size().x + 24.0;
                let btn = egui::Rect::from_min_size(
                    egui::pos2(anchor_x - w, cy - 12.0),
                    egui::vec2(w, 24.0),
                );
                let resp = ui.interact(btn, id, egui::Sense::click());
                if resp.clicked() {
                    bump(outcome, self.do_action(spec.field));
                }
                let border = if selected || resp.hovered() {
                    BORDER_FOCUS
                } else {
                    BORDER
                };
                ui.painter().rect_stroke(
                    btn,
                    egui::CornerRadius::same(4),
                    egui::Stroke::new(1.0, border),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    btn.center(),
                    egui::Align2::CENTER_CENTER,
                    caption,
                    egui::FontId::proportional(12.5),
                    TEXT,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DefaultShell, Settings};

    #[test]
    fn every_pane_has_rows_and_labels() {
        for p in Pane::ALL {
            assert!(!p.label().is_empty());
            assert!(!rows(p).is_empty(), "{:?} has no rows", p);
        }
    }

    #[test]
    fn toggle_flips_and_reports_change() {
        let mut s = Settings::default();
        assert!(adjust(Field::CopyOnSelect, Adjust::Toggle, &mut s));
        assert!(s.copy_on_select);
        assert!(adjust(Field::CopyOnSelect, Adjust::Toggle, &mut s));
        assert!(!s.copy_on_select);
    }

    #[test]
    fn stepper_clamps_at_bounds_and_reports_no_change() {
        let mut s = Settings::default();
        s.send_settle_ms = 2000;
        assert!(
            !adjust(Field::SendSettle, Adjust::Inc, &mut s),
            "inc at max is a no-op"
        );
        assert_eq!(s.send_settle_ms, 2000);
        s.scroll_speed = 1.0;
        assert!(!adjust(Field::ScrollSpeed, Adjust::Dec, &mut s));
        assert_eq!(s.scroll_speed, 1.0);
    }

    #[test]
    fn shell_choice_cycles_through_all_variants() {
        let mut s = Settings::default();
        adjust(Field::DefaultShellF, Adjust::Inc, &mut s);
        assert_eq!(s.default_shell, DefaultShell::Cmd);
        adjust(Field::DefaultShellF, Adjust::Inc, &mut s);
        assert_eq!(s.default_shell, DefaultShell::Sh);
        adjust(Field::DefaultShellF, Adjust::Inc, &mut s);
        assert_eq!(s.default_shell, DefaultShell::PowerShell, "wraps");
    }

    #[test]
    fn nav_stays_in_bounds_and_tab_switches_focus() {
        let mut m = SettingsMenu::new();
        assert!(m.in_rail);
        m.nav_tab();
        assert!(!m.in_rail);
        m.nav_up(); // row 0 → stays 0 (no wrap; matches keymap editor feel)
        assert_eq!(m.row, 0);
        let last = rows(m.pane).len() - 1;
        for _ in 0..rows(m.pane).len() + 5 {
            m.nav_down();
        }
        assert_eq!(m.row, last, "clamps at last row");
    }

    #[test]
    fn display_formats_units() {
        let s = Settings::default();
        assert_eq!(display(Field::SendSettle, &s), "120 ms");
        assert_eq!(display(Field::ScrollbackLines, &s), "10000");
        assert_eq!(display(Field::BellPeriod, &s), "1.2 s");
        assert_eq!(display(Field::DefaultShellF, &s), "PowerShell");
    }

    #[test]
    fn crew_stale_displays_minutes_when_divisible_by_60() {
        let mut s = Settings::default();
        s.crew_stale_secs = 300;
        assert_eq!(display(Field::CrewStale, &s), "5 min");
        s.crew_stale_secs = 90;
        assert_eq!(display(Field::CrewStale, &s), "90 s");
    }

    #[test]
    fn check_updates_now_action_requests_a_check() {
        let m = SettingsMenu::new();
        assert!(matches!(
            m.do_action(Field::CheckUpdatesNow),
            MenuOutcome::CheckUpdatesNow
        ));
    }

    #[test]
    fn check_updates_now_outranks_changed_but_not_close() {
        let mut o = MenuOutcome::Changed;
        bump(&mut o, MenuOutcome::CheckUpdatesNow);
        assert!(matches!(o, MenuOutcome::CheckUpdatesNow));
        bump(&mut o, MenuOutcome::Close);
        assert!(matches!(o, MenuOutcome::Close));
    }
}
