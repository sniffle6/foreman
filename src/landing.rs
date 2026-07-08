use eframe::egui;
use std::path::PathBuf;

use crate::dirpicker::{DirPicker, Outcome};
use crate::icons::{self, IconKind};
use crate::recents::RecentEntry;
use crate::theme::{BORDER_FOCUS, DIM, SEL_BG, TEXT, WIN_BG};

/// Provisional, landing-local taxonomy (phase-2 replaces it with the dispatch model).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionKind {
    Claude,
    Codex,
    Terminal,
}

/// Open `path` as a new project running `kind`.
pub struct LandingAction {
    pub path: PathBuf,
    pub kind: SessionKind,
}

/// Fixed icon order so positional rects and the hit-test agree.
const ICON_ORDER: [SessionKind; 3] = [
    SessionKind::Claude,
    SessionKind::Codex,
    SessionKind::Terminal,
];

/// FOREMAN in a mono block font — real terminal art.
const FOREMAN_ART: &str = r"
███████  ██████  ██████  ███████ ███    ███  █████  ███    ██
██      ██    ██ ██   ██ ██      ████  ████ ██   ██ ████   ██
█████   ██    ██ ██████  █████   ██ ████ ██ ███████ ██ ██  ██
██      ██    ██ ██   ██ ██      ██  ██  ██ ██   ██ ██  ██ ██
██       ██████  ██   ██ ███████ ██      ██ ██   ██ ██   ████";

struct LandingLayout {
    wordmark: egui::Rect,
    tagline: egui::Rect,
    field: egui::Rect,
    icons: Vec<egui::Rect>,
    recents_header: egui::Rect,
    recents: Vec<egui::Rect>,
}

/// Recents band metrics (below the icon row, which paints labels ~24px under
/// the icons — BAND_GAP clears them).
const BAND_GAP: f32 = 44.0;
const HEADER_H: f32 = 18.0;
const ROW_H: f32 = 24.0;
const ROW_GAP: f32 = 4.0;

/// Place the stack (wordmark → tagline → field → icon row → recents) centered
/// in `area`. Pure arithmetic — no fonts, no fs.
fn layout(area: egui::Rect, n_icons: usize, n_recents: usize) -> LandingLayout {
    let cx = area.center().x;
    let field_w = area.width().min(520.0).max(0.0);
    let (word_h, tag_h, field_h, icon, gap) = (120.0_f32, 24.0, 26.0, 72.0_f32, 18.0);
    let recents_h = if n_recents > 0 {
        BAND_GAP + HEADER_H + 6.0 + ROW_H * n_recents as f32 + ROW_GAP * (n_recents as f32 - 1.0)
    } else {
        0.0
    };
    let total = word_h + 16.0 + tag_h + 28.0 + field_h + 36.0 + icon + recents_h;
    let mut y = area.center().y - total / 2.0;

    let centered = |w: f32, y: f32, h: f32| {
        egui::Rect::from_min_size(egui::pos2(cx - w / 2.0, y), egui::vec2(w, h))
    };
    let word_w = area.width().min(760.0);
    let wordmark = centered(word_w, y, word_h);
    y += word_h + 16.0;
    let tagline = centered(word_w, y, tag_h);
    y += tag_h + 28.0;
    let field = centered(field_w, y, field_h);
    y += field_h + 36.0;

    let n = n_icons.max(1);
    let row_w = (icon * n as f32 + gap * (n as f32 - 1.0)).min(area.width());
    let mut x = cx - row_w / 2.0;
    let icons = (0..n_icons)
        .map(|_| {
            let r = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(icon, icon));
            x += icon + gap;
            r
        })
        .collect();
    y += icon;

    let (recents_header, recents) = if n_recents > 0 {
        y += BAND_GAP;
        let header = centered(field_w, y, HEADER_H);
        y += HEADER_H + 6.0;
        let mut rows = Vec::with_capacity(n_recents);
        for _ in 0..n_recents {
            rows.push(centered(field_w, y, ROW_H));
            y += ROW_H + ROW_GAP;
        }
        (header, rows)
    } else {
        (egui::Rect::NOTHING, Vec::new())
    };

    LandingLayout {
        wordmark,
        tagline,
        field,
        icons,
        recents_header,
        recents,
    }
}

/// Which landing area owns navigation keys. One vertical axis, top to bottom:
/// the picker's text field (default), the agent buttons, the recents list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Zone {
    Field,
    Buttons,
    Recents,
}

/// Keyboard cursor across the three zones: the zone plus a remembered
/// selection inside each (button index, recent-row index).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Nav {
    zone: Zone,
    btn: usize,
    rec: usize,
}

impl Nav {
    const HOME: Nav = Nav {
        zone: Zone::Field,
        btn: 0,
        rec: 0,
    };
}

/// What a key step asks the landing to do beyond moving the cursor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StepAct {
    None,
    /// ↓ in the field: open the directory popup.
    OpenPopup,
    /// Enter on an agent button: launch that kind at the field's path.
    LaunchButton(usize),
    /// Enter on a recents row: reopen that entry.
    OpenRecent(usize),
}

#[derive(Clone, Copy, Debug)]
enum NavKey {
    Tab,
    ShiftTab,
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Text,
}

/// Pure keyboard model. Tab / Shift+Tab cycle the zones forward / backward
/// (recents skipped while empty); ↑/↓ walk the same order as one clamped
/// vertical axis, with ↓ in the field opening the directory popup instead.
/// Esc or typing returns to the field. Enter acts on the current zone.
fn step(nav: Nav, n_btns: usize, n_recs: usize, key: NavKey) -> (Nav, StepAct) {
    use NavKey::*;
    use StepAct::None as Move;
    let Nav { zone, btn, rec } = nav;
    let to = |zone| (Nav { zone, btn, rec }, Move);
    match (zone, key) {
        (Zone::Field, Tab) => to(Zone::Buttons),
        (Zone::Field, ShiftTab) if n_recs > 0 => to(Zone::Recents),
        (Zone::Field, ShiftTab) => to(Zone::Buttons),
        (Zone::Field, Down) => (nav, StepAct::OpenPopup),
        (Zone::Field, _) => (nav, Move),

        (Zone::Buttons, Tab) if n_recs > 0 => to(Zone::Recents),
        (Zone::Buttons, Tab) => to(Zone::Field),
        (Zone::Buttons, ShiftTab | Up | Esc | Text) => to(Zone::Field),
        (Zone::Buttons, Left) => (
            Nav {
                zone,
                btn: btn.saturating_sub(1),
                rec,
            },
            Move,
        ),
        (Zone::Buttons, Right) => (
            Nav {
                zone,
                btn: (btn + 1).min(n_btns.saturating_sub(1)),
                rec,
            },
            Move,
        ),
        (Zone::Buttons, Down) if n_recs > 0 => to(Zone::Recents),
        (Zone::Buttons, Down) => (nav, Move),
        (Zone::Buttons, Enter) => (nav, StepAct::LaunchButton(btn)),

        (Zone::Recents, Up) if rec > 0 => (
            Nav {
                zone,
                btn,
                rec: rec - 1,
            },
            Move,
        ),
        (Zone::Recents, Up) => to(Zone::Buttons),
        (Zone::Recents, Down) => (
            Nav {
                zone,
                btn,
                rec: (rec + 1).min(n_recs.saturating_sub(1)),
            },
            Move,
        ),
        (Zone::Recents, Tab | Esc | Text) => to(Zone::Field),
        (Zone::Recents, ShiftTab) => to(Zone::Buttons),
        (Zone::Recents, Enter) => (Nav { zone: Zone::Field, btn, rec }, StepAct::OpenRecent(rec)),
        (Zone::Recents, Left | Right) => (nav, Move),
    }
}

fn icon_of(k: SessionKind) -> IconKind {
    match k {
        SessionKind::Claude => IconKind::Claude,
        SessionKind::Codex => IconKind::Codex,
        SessionKind::Terminal => IconKind::PowerShell, // shared shell-prompt glyph
    }
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Per-glyph wordmark color: a monotonic ember→amber→gold ramp across x, lit
/// from the top (vertical bevel), with a moving specular streak blended toward
/// warm white where `shine` is high. All stops sit in the warm terminal palette
/// (signature amber `231,169,63`) so it reads as lit metal, not a cool gradient.
fn wordmark_color(fx: f32, fy: f32, shine: f32) -> egui::Color32 {
    const EMBER: [f32; 3] = [196.0, 82.0, 44.0]; // left
    const AMBER: [f32; 3] = [232.0, 150.0, 56.0]; // mid
    const GOLD: [f32; 3] = [242.0, 198.0, 98.0]; // right
    let base = if fx < 0.5 {
        lerp3(EMBER, AMBER, fx / 0.5)
    } else {
        lerp3(AMBER, GOLD, (fx - 0.5) / 0.5)
    };
    let shade = 1.08 - 0.24 * fy; // top-lit bevel: brighter up top, darker below
    let lit = [base[0] * shade, base[1] * shade, base[2] * shade];
    const HILITE: [f32; 3] = [255.0, 249.0, 233.0];
    let c = lerp3(lit, HILITE, shine.clamp(0.0, 1.0));
    egui::Color32::from_rgb(
        c[0].clamp(0.0, 255.0).round() as u8,
        c[1].clamp(0.0, 255.0).round() as u8,
        c[2].clamp(0.0, 255.0).round() as u8,
    )
}

/// Center of the moving specular streak (in x-fraction) for animation time
/// `time` seconds. A quick sweep across the wordmark, then a long rest with the
/// streak parked off-screen — returns `f32::INFINITY` during the rest.
fn shine_center(time: f32) -> f32 {
    const PERIOD: f32 = 5.0; // one sweep every 5s
    const SWEEP: f32 = 0.42; // fraction of the period the streak is travelling
    let phase = (time % PERIOD) / PERIOD;
    if phase >= SWEEP {
        return f32::INFINITY; // parked off-screen: resting gradient
    }
    let u = phase / SWEEP; // 0..1 across the sweep
    let eased = u * u * (3.0 - 2.0 * u); // smoothstep
    -0.25 + 1.5 * eased // travel from just-left to just-right of the glyphs
}

/// Paint the block-art wordmark: forge gradient per glyph, an animated specular
/// sweep, and a soft warm bloom behind. Centered in `rect`, top-aligned.
fn paint_wordmark(ui: &egui::Ui, rect: egui::Rect, art: &str, font: egui::FontId, time: f32) {
    let lines: Vec<&str> = art.lines().collect();
    let rows = lines.len().max(1);
    let cols = lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(1)
        .max(1);

    let p = shine_center(time);
    const WIDTH: f32 = 0.11; // streak half-width in x-fraction
    const INTENSITY: f32 = 0.9;

    let mut job = egui::text::LayoutJob::default();
    let mut buf = [0u8; 4];
    for (row, line) in lines.iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            let fx = if cols > 1 {
                col as f32 / (cols - 1) as f32
            } else {
                0.0
            };
            let fy = if rows > 1 {
                row as f32 / (rows - 1) as f32
            } else {
                0.0
            };
            // Gaussian streak, skewed slightly by row so the shine falls on a
            // diagonal (light glinting across the face of the letters).
            let d = (fx + 0.10 * fy - p) / WIDTH;
            let shine = if p.is_finite() {
                INTENSITY * (-(d * d)).exp()
            } else {
                0.0
            };
            job.append(
                ch.encode_utf8(&mut buf),
                0.0,
                egui::text::TextFormat {
                    font_id: font.clone(),
                    color: wordmark_color(fx, fy, shine),
                    ..Default::default()
                },
            );
        }
        if row + 1 < rows {
            job.append(
                "\n",
                0.0,
                egui::text::TextFormat {
                    font_id: font.clone(),
                    ..Default::default()
                },
            );
        }
    }

    let galley = ui.painter().layout_job(job);
    let pos = egui::pos2(rect.center().x - galley.size().x / 2.0, rect.top());

    // Warm bloom: a few low-alpha amber copies offset around the glyphs, so the
    // wordmark glows off the dark surface. Then the crisp gradient on top.
    let glow = egui::Color32::from_rgba_unmultiplied(233, 138, 48, 34);
    for off in [
        egui::vec2(-1.6, 0.0),
        egui::vec2(1.6, 0.0),
        egui::vec2(0.0, -1.6),
        egui::vec2(0.0, 1.6),
    ] {
        ui.painter()
            .galley_with_override_text_color(pos + off, galley.clone(), glow);
    }
    ui.painter().galley(pos, galley, TEXT);
}

impl SessionKind {
    /// Human label for buttons and notifications.
    pub fn label(self) -> &'static str {
        match self {
            SessionKind::Claude => "Claude",
            SessionKind::Codex => "Codex",
            SessionKind::Terminal => "Terminal",
        }
    }

    /// Map a persisted recents kind string back to a kind. Unknown strings (a
    /// future agent written by a newer build) degrade to Terminal — per the
    /// recents spec, one bad entry must never cost the list.
    pub fn from_kind_str(s: &str) -> SessionKind {
        match s {
            "claude" => SessionKind::Claude,
            "codex" => SessionKind::Codex,
            _ => SessionKind::Terminal,
        }
    }

    /// PATH stem to probe and launch, or None for a plain shell.
    fn stem(self) -> Option<&'static str> {
        match self {
            SessionKind::Claude => Some("claude"),
            SessionKind::Codex => Some("codex"),
            SessionKind::Terminal => None,
        }
    }

    /// The command to type into the project's shell to start this session, or
    /// None for a plain shell (Terminal). Running the agent inside a normal
    /// shell means quitting it drops back to a prompt instead of closing the
    /// pane. Same string as the PATH stem today.
    pub fn launch_command(self) -> Option<&'static str> {
        self.stem()
    }

    /// Is this agent resolvable on PATH? A plain shell is always "installed".
    pub fn installed(self) -> bool {
        match self.stem() {
            None => true,
            Some(s) => on_path(s, std::env::var_os("PATH").as_deref(), &|p| p.exists()),
        }
    }
}

/// Pure: is `stem` (with any cmd-runnable extension) present in a PATH dir?
/// Inject the PATH value and an existence probe — mirrors `preferred_powershell`
/// in terminal.rs, so tests never touch the real filesystem.
fn on_path(
    stem: &str,
    path: Option<&std::ffi::OsStr>,
    exists: &dyn Fn(&std::path::Path) -> bool,
) -> bool {
    let Some(path) = path else {
        return false;
    };
    // Extensions the shell can launch (npm ships .cmd/.ps1 shims); "" catches
    // an exact-named executable.
    const EXTS: [&str; 5] = ["", ".exe", ".cmd", ".bat", ".ps1"];
    std::env::split_paths(path).any(|dir| {
        EXTS.iter()
            .any(|ext| exists(&dir.join(format!("{stem}{ext}"))))
    })
}

/// The empty-desktop landing. Owns its own path-field picker (separate from the
/// desktop's leader picker).
pub struct Landing {
    picker: DirPicker,
    nav: Nav,
    /// Missing-dir-filtered recents snapshot; rebuilt when `refilter` is set.
    visible: Vec<RecentEntry>,
    refilter: bool,
}

impl Landing {
    pub fn new(start: PathBuf) -> Self {
        Self {
            picker: DirPicker::new(start),
            nav: Nav::HOME,
            visible: Vec::new(),
            refilter: true,
        }
    }

    /// Re-open + re-focus the picker when the landing reappears (the same
    /// `Landing` lives for the app's lifetime, so its picker's one-shot focus
    /// flag is already spent after the first show).
    pub fn reopen(&mut self) {
        self.picker.reopen();
        self.nav = Nav::HOME;
        self.refilter = true; // recents may have changed while a project was open
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        area: egui::Rect,
        recents: &[RecentEntry],
    ) -> Option<LandingAction> {
        // Display-only filter: an entry whose dir is missing (unplugged drive) is
        // hidden, not deleted — it comes back when the drive does (spec). Cached
        // and refreshed on landing (re)appearance, NOT per frame: the landing
        // repaints continuously for the wordmark sweep, and is_dir() on a dead
        // network path can block the GUI thread for seconds.
        if std::mem::take(&mut self.refilter) {
            self.visible = recents.iter().filter(|e| e.path.is_dir()).cloned().collect();
        }
        let visible = &self.visible;
        let l = layout(area, ICON_ORDER.len(), visible.len());

        let mut action: Option<LandingAction> = None;
        if visible.is_empty() && self.nav.zone == Zone::Recents {
            self.nav = Nav::HOME; // the list emptied under the cursor
        }
        if !self.picker.is_open() {
            let key = ui.input_mut(|i| {
                if i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab) {
                    return Some(NavKey::ShiftTab);
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Tab) {
                    return Some(NavKey::Tab);
                }
                match self.nav.zone {
                    // The field owns editing keys; only ↓ (open the popup) is
                    // claimed while the popup is closed.
                    Zone::Field => {
                        if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                            return Some(NavKey::Down);
                        }
                    }
                    Zone::Buttons | Zone::Recents => {
                        if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                            return Some(NavKey::Up);
                        }
                        if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                            return Some(NavKey::Down);
                        }
                        if self.nav.zone == Zone::Buttons {
                            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
                                return Some(NavKey::Left);
                            }
                            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
                                return Some(NavKey::Right);
                            }
                        }
                        if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                            return Some(NavKey::Enter);
                        }
                        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                            return Some(NavKey::Esc);
                        }
                        if i.events.iter().any(|e| matches!(e, egui::Event::Text(_))) {
                            return Some(NavKey::Text); // typing always means "edit the path"
                        }
                    }
                }
                None
            });
            if let Some(key) = key {
                let (nav, act) = step(self.nav, ICON_ORDER.len(), visible.len(), key);
                self.nav = nav;
                match act {
                    StepAct::None => {}
                    StepAct::OpenPopup => self.picker.open_dropdown(),
                    StepAct::LaunchButton(i) => {
                        if let Some(path) = self.picker.current_dir() {
                            action = Some(LandingAction {
                                path,
                                kind: ICON_ORDER[i],
                            });
                        }
                    }
                    StepAct::OpenRecent(i) => {
                        let e = &visible[i];
                        action = Some(LandingAction {
                            path: e.path.clone(),
                            kind: SessionKind::from_kind_str(&e.kind),
                        });
                    }
                }
            }
        } else {
            self.nav = Nav::HOME; // popup open: the field owns every key
        }

        // Wordmark (mono block art, forge-gradient with an animated specular
        // sweep) + tagline, centered. The sweep needs a steady repaint while the
        // landing is up; it stops as soon as a project opens (show stops being
        // called), so the idle cost is scoped to the empty desktop.
        let word_font = egui::FontId::monospace(14.0);
        let time = ui.input(|i| i.time) as f32;
        paint_wordmark(
            ui,
            l.wordmark,
            FOREMAN_ART.trim_matches('\n'),
            word_font,
            time,
        );
        ui.ctx().request_repaint();
        ui.painter().text(
            l.tagline.center(),
            egui::Align2::CENTER_CENTER,
            "tmux for AI agents",
            egui::FontId::proportional(14.0),
            DIM,
        );

        // Inline picker in the field rect.
        let picker_out = {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(l.field));
            self.picker.show(&mut child)
        };
        match picker_out {
            Outcome::Accepted(path) => {
                action = Some(LandingAction {
                    path,
                    kind: SessionKind::Terminal,
                });
            }
            // ↓ past the popup's last row exits downward, onto the buttons.
            Outcome::PassedEnd => self.nav.zone = Zone::Buttons,
            _ => {}
        }

        // Icon row — each opens the picker's current path with that kind.
        // Landing-drawn hit areas, NOT egui Buttons: focusable widgets would
        // catch egui's Tab focus-traversal and fight the zone model (the
        // double-selection bug); here the `Nav` cursor is the only selection.
        for (idx, (r, &kind)) in l.icons.iter().zip(ICON_ORDER.iter()).enumerate() {
            let resp = ui.interact(*r, ui.id().with(("icon", idx)), egui::Sense::click());
            let selected = self.nav.zone == Zone::Buttons && idx == self.nav.btn;
            ui.painter()
                .rect_filled(*r, egui::CornerRadius::same(4), WIN_BG);
            if selected || resp.hovered() {
                ui.painter()
                    .rect_filled(*r, egui::CornerRadius::same(4), SEL_BG);
            }
            if selected {
                ui.painter().rect_stroke(
                    *r,
                    egui::CornerRadius::same(4),
                    egui::Stroke::new(1.0, BORDER_FOCUS),
                    egui::StrokeKind::Inside,
                );
            }
            let tex = icons::texture(ui.ctx(), icon_of(kind), 48);
            ui.painter().image(
                tex.id(),
                egui::Rect::from_center_size(r.center(), egui::vec2(48.0, 48.0)),
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
            ui.painter().text(
                r.center_bottom() + egui::vec2(0.0, 12.0),
                egui::Align2::CENTER_TOP,
                kind.label(),
                egui::FontId::proportional(12.0),
                TEXT,
            );
            if resp.clicked() {
                if let Some(path) = self.picker.current_dir() {
                    action = Some(LandingAction { path, kind });
                }
            }
        }

        if !visible.is_empty() {
            ui.painter().text(
                l.recents_header.left_center(),
                egui::Align2::LEFT_CENTER,
                "Recent",
                egui::FontId::proportional(12.0),
                DIM,
            );
            let row_font = egui::FontId::proportional(13.0);
            for (idx, (r, e)) in l.recents.iter().zip(visible.iter()).enumerate() {
                let resp = ui.interact(*r, ui.id().with(("recent", idx)), egui::Sense::click());
                let selected = self.nav.zone == Zone::Recents && idx == self.nav.rec;
                if selected || resp.hovered() {
                    ui.painter()
                        .rect_filled(*r, egui::CornerRadius::same(3), SEL_BG);
                }
                if selected {
                    ui.painter().text(
                        egui::pos2(r.min.x + 6.0, r.center().y),
                        egui::Align2::LEFT_CENTER,
                        ">",
                        egui::FontId::monospace(13.0),
                        TEXT,
                    );
                }
                let kind = SessionKind::from_kind_str(&e.kind);
                let tex = icons::texture(ui.ctx(), icon_of(kind), 16);
                let icon_rect = egui::Rect::from_center_size(
                    egui::pos2(r.min.x + 28.0, r.center().y),
                    egui::vec2(16.0, 16.0),
                );
                ui.painter().image(
                    tex.id(),
                    icon_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                let name = e
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| e.path.display().to_string());
                let name_x = r.min.x + 44.0;
                let name_w = ui
                    .painter()
                    .layout_no_wrap(name.clone(), row_font.clone(), TEXT)
                    .rect
                    .width();
                ui.painter().text(
                    egui::pos2(name_x, r.center().y),
                    egui::Align2::LEFT_CENTER,
                    name,
                    row_font.clone(),
                    TEXT,
                );
                if let Some(parent) = e.path.parent() {
                    ui.painter().text(
                        egui::pos2(name_x + name_w + 10.0, r.center().y),
                        egui::Align2::LEFT_CENTER,
                        parent.display().to_string(),
                        row_font.clone(),
                        DIM,
                    );
                }
                if resp.clicked() {
                    action = Some(LandingAction {
                        path: e.path.clone(),
                        kind,
                    });
                }
            }
        }

        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1200.0, 800.0))
    }

    #[test]
    fn every_element_is_inside_the_area_and_disjoint() {
        let a = area();
        let l = layout(a, 3, 0);
        for r in [l.wordmark, l.tagline, l.field] {
            assert!(a.contains_rect(r), "{r:?} escapes {a:?}");
        }
        assert!(l.wordmark.bottom() <= l.tagline.top());
        assert!(l.tagline.bottom() <= l.field.top());
        assert!(l.field.bottom() <= l.icons[0].top());
    }

    #[test]
    fn stack_is_horizontally_centered() {
        let a = area();
        let l = layout(a, 3, 0);
        let c = a.center().x;
        for r in [l.wordmark, l.tagline, l.field] {
            assert!((r.center().x - c).abs() < 1.0, "{r:?} not centered");
        }
    }

    #[test]
    fn icons_are_equal_width_and_evenly_spaced() {
        let l = layout(area(), 3, 0);
        assert_eq!(l.icons.len(), 3);
        let w = l.icons[0].width();
        assert!(l.icons.iter().all(|r| (r.width() - w).abs() < 0.5));
        let g1 = l.icons[1].left() - l.icons[0].right();
        let g2 = l.icons[2].left() - l.icons[1].right();
        assert!((g1 - g2).abs() < 0.5);
    }

    #[test]
    fn tiny_area_degrades_without_negative_sizes() {
        let a = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(240.0, 180.0));
        let l = layout(a, 3, 0);
        for r in [l.wordmark, l.tagline, l.field] {
            assert!(r.width() >= 0.0 && r.height() >= 0.0);
            assert!(a.contains_rect(r.intersect(a)));
        }
    }

    #[test]
    fn launch_command_names_agents_and_skips_shells() {
        assert_eq!(SessionKind::Claude.launch_command(), Some("claude"));
        assert_eq!(SessionKind::Codex.launch_command(), Some("codex"));
        assert_eq!(SessionKind::Terminal.launch_command(), None);
    }

    #[test]
    fn on_path_resolves_a_shim_by_extension() {
        let path = std::env::join_paths(["/a", "/b"]).unwrap();
        // Only `claude.cmd` exists; probe matches on the .cmd extension.
        let has_claude_cmd =
            |p: &std::path::Path| p.file_name().and_then(|n| n.to_str()) == Some("claude.cmd");
        assert!(on_path("claude", Some(path.as_os_str()), &has_claude_cmd));
        assert!(!on_path("codex", Some(path.as_os_str()), &has_claude_cmd));
    }

    #[test]
    fn on_path_is_false_without_a_path_var() {
        assert!(!on_path("claude", None, &|_| true));
    }

    #[test]
    fn recents_band_sits_below_icons_inside_area() {
        let a = area();
        let l = layout(a, 3, 4);
        assert_eq!(l.recents.len(), 4);
        let icons_bottom = l.icons.iter().map(|r| r.bottom()).fold(f32::MIN, f32::max);
        assert!(
            l.recents_header.top() > icons_bottom,
            "band below the icon row"
        );
        assert!(a.contains_rect(l.recents_header));
        for r in &l.recents {
            assert!(a.contains_rect(*r));
        }
        for w in l.recents.windows(2) {
            assert!(w[1].top() >= w[0].bottom(), "rows don't overlap");
            assert_eq!(w[0].height(), w[1].height(), "rows equal height");
        }
    }

    #[test]
    fn zero_recents_hides_the_band() {
        let l = layout(area(), 3, 0);
        assert!(l.recents.is_empty());
    }

    fn nav(zone: Zone, btn: usize, rec: usize) -> Nav {
        Nav { zone, btn, rec }
    }

    #[test]
    fn tab_cycles_zones_forward_and_shift_tab_backward() {
        let (n, _) = step(Nav::HOME, 3, 2, NavKey::Tab);
        assert_eq!(n.zone, Zone::Buttons);
        let (n, _) = step(n, 3, 2, NavKey::Tab);
        assert_eq!(n.zone, Zone::Recents);
        let (n, _) = step(n, 3, 2, NavKey::Tab);
        assert_eq!(n.zone, Zone::Field);
        let (n, _) = step(n, 3, 2, NavKey::ShiftTab);
        assert_eq!(n.zone, Zone::Recents);
        let (n, _) = step(n, 3, 2, NavKey::ShiftTab);
        assert_eq!(n.zone, Zone::Buttons);
        let (n, _) = step(n, 3, 2, NavKey::ShiftTab);
        assert_eq!(n.zone, Zone::Field);
    }

    #[test]
    fn empty_recents_is_skipped_in_both_directions_and_below_buttons() {
        let (n, _) = step(nav(Zone::Buttons, 0, 0), 3, 0, NavKey::Tab);
        assert_eq!(n.zone, Zone::Field);
        let (n, _) = step(Nav::HOME, 3, 0, NavKey::ShiftTab);
        assert_eq!(n.zone, Zone::Buttons);
        let (n, _) = step(nav(Zone::Buttons, 1, 0), 3, 0, NavKey::Down);
        assert_eq!((n.zone, n.btn), (Zone::Buttons, 1), "clamp: nothing below");
    }

    #[test]
    fn vertical_axis_walks_field_buttons_recents_with_clamps() {
        let (n, act) = step(Nav::HOME, 3, 2, NavKey::Down);
        assert_eq!((n.zone, act), (Zone::Field, StepAct::OpenPopup));
        let (n, _) = step(nav(Zone::Buttons, 1, 0), 3, 2, NavKey::Up);
        assert_eq!(n.zone, Zone::Field);
        let (n, _) = step(nav(Zone::Buttons, 1, 0), 3, 2, NavKey::Down);
        assert_eq!(n.zone, Zone::Recents);
        let (n, _) = step(nav(Zone::Recents, 0, 0), 3, 2, NavKey::Up);
        assert_eq!(n.zone, Zone::Buttons);
        let (n, _) = step(nav(Zone::Recents, 0, 0), 3, 2, NavKey::Down);
        assert_eq!((n.zone, n.rec), (Zone::Recents, 1));
        let (n, _) = step(nav(Zone::Recents, 0, 1), 3, 2, NavKey::Down);
        assert_eq!(n.rec, 1, "clamp at the last recent");
        let (n, _) = step(Nav::HOME, 3, 2, NavKey::Up);
        assert_eq!(n.zone, Zone::Field, "clamp at the top");
    }

    #[test]
    fn buttons_left_right_clamp_and_enter_launches() {
        let (n, _) = step(nav(Zone::Buttons, 0, 0), 3, 2, NavKey::Left);
        assert_eq!(n.btn, 0, "clamp left");
        let (n, _) = step(nav(Zone::Buttons, 0, 0), 3, 2, NavKey::Right);
        assert_eq!(n.btn, 1);
        let (n, _) = step(nav(Zone::Buttons, 2, 0), 3, 2, NavKey::Right);
        assert_eq!(n.btn, 2, "clamp right");
        let (_, act) = step(nav(Zone::Buttons, 1, 0), 3, 2, NavKey::Enter);
        assert_eq!(act, StepAct::LaunchButton(1));
    }

    #[test]
    fn enter_opens_recent_and_esc_text_return_to_field() {
        let (n, act) = step(nav(Zone::Recents, 0, 1), 3, 2, NavKey::Enter);
        assert_eq!((n.zone, act), (Zone::Field, StepAct::OpenRecent(1)));
        let (n, _) = step(nav(Zone::Recents, 0, 1), 3, 2, NavKey::Esc);
        assert_eq!(n.zone, Zone::Field);
        let (n, _) = step(nav(Zone::Buttons, 1, 0), 3, 2, NavKey::Text);
        assert_eq!(n.zone, Zone::Field);
    }

    #[test]
    fn kind_strings_map_back_with_unknown_falling_to_terminal() {
        assert_eq!(SessionKind::from_kind_str("claude"), SessionKind::Claude);
        assert_eq!(SessionKind::from_kind_str("codex"), SessionKind::Codex);
        assert_eq!(
            SessionKind::from_kind_str("terminal"),
            SessionKind::Terminal
        );
        assert_eq!(
            SessionKind::from_kind_str("future-agent"),
            SessionKind::Terminal
        );
    }
}
