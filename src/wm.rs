use crate::terminal::{Session, Shell};
use eframe::egui;

pub type WinId = u64;

const DESK_BG: egui::Color32 = egui::Color32::from_rgb(25, 23, 19);
const WIN_BG: egui::Color32 = egui::Color32::from_rgb(33, 30, 24);
const TITLE_BG: egui::Color32 = egui::Color32::from_rgb(43, 39, 31);
const TITLE_BG_FOCUS: egui::Color32 = egui::Color32::from_rgb(56, 49, 36);
// Project windows (the outer nesting level) get a subtly cooler, deeper titlebar
// so the two levels read as distinct without breaking the warm-graphite look.
const PROJ_TITLE_BG: egui::Color32 = egui::Color32::from_rgb(37, 41, 39);
const PROJ_TITLE_BG_FOCUS: egui::Color32 = egui::Color32::from_rgb(48, 56, 52);
const BORDER: egui::Color32 = egui::Color32::from_rgb(60, 55, 45);
const BORDER_FOCUS: egui::Color32 = egui::Color32::from_rgb(231, 169, 63);
const TEXT: egui::Color32 = egui::Color32::from_rgb(222, 222, 212);
const DIM: egui::Color32 = egui::Color32::from_rgb(150, 143, 125);

const TITLE_H: f32 = 26.0;

// snap overlay (amber, matches BORDER_FOCUS / web mockup --needs #e7a93f)
const SNAP_FILL: egui::Color32 = egui::Color32::from_rgba_premultiplied(231, 169, 63, 33); // ~13% alpha
const SNAP_STROKE: egui::Color32 = egui::Color32::from_rgb(231, 169, 63);
const SNAP_GAP: f32 = 8.0; // inset of zones from the area edge (web mockup `g`)
const TOP_HOLD: f64 = 0.6; // hold in the top zone this long (s) → escalate to maximize
const GROW_LEAD: f64 = 0.25; // overlay grows top-half → full over the last GROW_LEAD secs

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Zone {
    Max,
    Left,
    Right,
    Top,
    Bottom,
    Tl,
    Tr,
    Bl,
    Br,
}

// pointer position as a fraction of the area → target zone (ports `detectZone`).
fn detect_zone(fx: f32, fy: f32) -> Option<Zone> {
    const T: f32 = 0.085; // edge band
    const C: f32 = 0.22; // corner band on the cross axis
    if fy < T {
        if fx < C {
            return Some(Zone::Tl);
        }
        if fx > 1.0 - C {
            return Some(Zone::Tr);
        }
        return Some(Zone::Top); // holding here escalates to Max (see resolve_zone)
    }
    if fy > 1.0 - T {
        if fx < C {
            return Some(Zone::Bl);
        }
        if fx > 1.0 - C {
            return Some(Zone::Br);
        }
        return Some(Zone::Bottom);
    }
    if fx < T {
        return Some(Zone::Left);
    }
    if fx > 1.0 - T {
        return Some(Zone::Right);
    }
    None
}

// target rect for a zone in LOCAL coords, inset by SNAP_GAP (ports `zoneRect`).
fn zone_rect(zone: Zone, area: egui::Vec2) -> egui::Rect {
    let g = SNAP_GAP;
    let (w, h) = (area.x, area.y);
    let hw = (w / 2.0 - g * 1.5).max(1.0); // half width, accounting for outer + center gap
    let hh = (h / 2.0 - g * 1.5).max(1.0);
    let rx = w / 2.0 + g / 2.0; // right-column x
    let by = h / 2.0 + g / 2.0; // bottom-row y
    let (x, y, sw, sh) = match zone {
        Zone::Max => (g, g, (w - g * 2.0).max(1.0), (h - g * 2.0).max(1.0)),
        Zone::Top => (g, g, (w - g * 2.0).max(1.0), hh),
        Zone::Bottom => (g, by, (w - g * 2.0).max(1.0), hh),
        Zone::Left => (g, g, hw, h - g * 2.0),
        Zone::Right => (rx, g, hw, h - g * 2.0),
        Zone::Tl => (g, g, hw, hh),
        Zone::Tr => (rx, g, hw, hh),
        Zone::Bl => (g, by, hw, hh),
        Zone::Br => (rx, by, hw, hh),
    };
    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(sw, sh))
}

fn lerp_rect(a: egui::Rect, b: egui::Rect, t: f32) -> egui::Rect {
    egui::Rect::from_min_max(a.min + (b.min - a.min) * t, a.max + (b.max - a.max) * t)
}

// Given the raw drag zone and how long it's been held, return the zone to commit
// plus the overlay rect to preview (local coords). The Top zone escalates to Max
// after TOP_HOLD seconds, the overlay growing top-half → full over GROW_LEAD.
fn resolve_zone(raw: Option<Zone>, held: f64, asz: egui::Vec2) -> (Option<Zone>, Option<egui::Rect>) {
    match raw {
        Some(Zone::Top) => {
            let p = (((held - (TOP_HOLD - GROW_LEAD)) / GROW_LEAD) as f32).clamp(0.0, 1.0);
            let ov = lerp_rect(zone_rect(Zone::Top, asz), zone_rect(Zone::Max, asz), p);
            let committed = if held >= TOP_HOLD { Zone::Max } else { Zone::Top };
            (Some(committed), Some(ov))
        }
        Some(z) => (Some(z), Some(zone_rect(z, asz))),
        None => (None, None),
    }
}

pub enum Content {
    Terminal(Session),
    /// A project window is a sandbox hosting its own nested WindowManager.
    Project(Box<WindowManager>),
}
impl Content {
    fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        active: bool,
        base: egui::Id,
        win_id: WinId,
        resp: &egui::Response,
    ) {
        match self {
            Content::Terminal(s) => s.show(ui, rect, active, resp),
            // Recurse: the project's content rect becomes the child manager's area.
            // The child only reads the keyboard if this project is itself active,
            // so `active` ANDs down the tree to exactly one leaf terminal.
            Content::Project(wm) => wm.show(ui, rect, active, base.with(("proj", win_id))),
        }
    }
}

pub struct Win {
    pub id: WinId,
    pub title: String,
    pub rect: egui::Rect, // local coords (origin = manager area.min)
    pub z: u64,
    pub minimized: bool,
    pub snap: Option<Zone>, // Some => tiled/maximized: refit to area each frame
    pub prev: Option<egui::Rect>, // floating rect to restore to when un-snapped
    pub content: Content,
}

enum Act {
    Focus(WinId),
    Close(WinId),
    Min(WinId),
    Max(WinId),
    Restore(WinId),
}

pub struct WindowManager {
    pub windows: Vec<Win>,
    z: u64,
    focused: Option<WinId>,
    next: WinId,
    dwell_zone: Option<Zone>, // raw zone the drag is currently hovering
    dwell_start: f64,         // time (s) the current dwell_zone was entered
}

impl WindowManager {
    pub fn new() -> Self {
        Self {
            windows: vec![],
            z: 1,
            focused: None,
            next: 1,
            dwell_zone: None,
            dwell_start: 0.0,
        }
    }

    // Cascading offset for a freshly spawned window, plus a fresh id + z.
    fn next_slot(&mut self, size: egui::Vec2) -> (WinId, egui::Rect) {
        let n = self.windows.len() as f32;
        let id = self.next;
        self.next += 1;
        self.z += 1;
        let off = 36.0 + 28.0 * (n % 6.0);
        let rect = egui::Rect::from_min_size(egui::pos2(off, off), size);
        (id, rect)
    }

    fn push_win(&mut self, id: WinId, title: String, rect: egui::Rect, content: Content) {
        self.windows.push(Win {
            id,
            title,
            rect,
            z: self.z,
            minimized: false,
            snap: None,
            prev: None,
            content,
        });
        self.focused = Some(id);
    }

    pub fn add_terminal(&mut self, shell: Shell, ctx: &egui::Context) {
        if let Ok(s) = Session::spawn(shell, ctx.clone()) {
            let (id, rect) = self.next_slot(egui::vec2(580.0, 380.0));
            self.push_win(
                id,
                format!("{}  ·  #{}", shell.label(), id),
                rect,
                Content::Terminal(s),
            );
        }
    }

    /// Add a new project window. It starts as a sandbox containing one terminal.
    /// TODO(status line): show repo / branch on the project titlebar.
    pub fn add_project(&mut self, shell: Shell, ctx: &egui::Context) {
        let (id, rect) = self.next_slot(egui::vec2(720.0, 480.0));
        let mut child = WindowManager::new();
        child.add_terminal(shell, ctx);
        self.push_win(
            id,
            format!("project {}", id),
            rect,
            Content::Project(Box::new(child)),
        );
    }

    /// Add a terminal inside the currently-focused project window. No-op if the
    /// focused window is not a project (or nothing is focused).
    pub fn add_terminal_to_focused(&mut self, shell: Shell, ctx: &egui::Context) {
        if let Some(fid) = self.focused {
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == fid) {
                if let Content::Project(wm) = &mut w.content {
                    wm.add_terminal(shell, ctx);
                }
            }
        }
    }

    fn focus(&mut self, id: WinId) {
        self.z += 1;
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            w.z = self.z;
        }
        self.focused = Some(id);
    }

    pub fn show(&mut self, ui: &mut egui::Ui, area: egui::Rect, active: bool, base: egui::Id) {
        ui.painter_at(area)
            .rect_filled(area, egui::CornerRadius::ZERO, DESK_BG);

        let focused = self.focused;
        let asz = area.size();
        let mut order: Vec<usize> = (0..self.windows.len())
            .filter(|&i| !self.windows[i].minimized)
            .collect();
        order.sort_by_key(|&i| self.windows[i].z);

        let mut acts: Vec<Act> = vec![];
        // overlay rect (screen coords) for the snap zone of the window being dragged
        let mut snap_overlay: Option<egui::Rect> = None;

        for &i in &order {
            let id = self.windows[i].id;
            let is_focus = focused == Some(id) && active;
            let is_project = matches!(self.windows[i].content, Content::Project(_));

            // Re-fit to the (possibly resized) area every frame: snapped/maximized
            // windows recompute to the new size; floating windows clamp back in.
            // This also confines freshly-created windows whose default size is
            // bigger than the area (e.g. a terminal spawned inside a small project).
            {
                let w = &mut self.windows[i];
                match w.snap {
                    Some(z) => w.rect = zone_rect(z, asz),
                    None => clamp(&mut w.rect, asz),
                }
            }
            let mut scr = self.windows[i].rect.translate(area.min.to_vec2());
            let ctl_w = 88.0;

            // --- title drag (interact first, then we know final position) ---
            let drag_rect =
                egui::Rect::from_min_size(scr.min, egui::vec2((scr.width() - ctl_w).max(0.0), TITLE_H));
            let dr = ui.interact(drag_rect, base.with((id, "drag")), egui::Sense::click_and_drag());
            if dr.drag_started() || dr.clicked() {
                acts.push(Act::Focus(id));
            }
            if dr.double_clicked() {
                acts.push(Act::Max(id));
            }
            if dr.dragged() {
                {
                    let w = &mut self.windows[i];
                    w.rect = w.rect.translate(dr.drag_delta());
                    w.snap = None; // dragging pops a snapped/maximized window back to floating
                    clamp(&mut w.rect, asz);
                }
                scr = self.windows[i].rect.translate(area.min.to_vec2());

                // --- snap: detect zone under the pointer (relative to area) ---
                // The top zone escalates to maximize the longer you hold it; the
                // overlay grows from top-half to full to telegraph the switch.
                if let Some(p) = ui.ctx().pointer_latest_pos() {
                    let fx = (p.x - area.min.x) / asz.x;
                    let fy = (p.y - area.min.y) / asz.y;
                    let now = ui.input(|inp| inp.time);
                    let raw = detect_zone(fx, fy);
                    if raw != self.dwell_zone {
                        self.dwell_zone = raw;
                        self.dwell_start = now;
                    }
                    let held = now - self.dwell_start;
                    let (_committed, overlay) = resolve_zone(raw, held, asz);
                    if let Some(r) = overlay {
                        snap_overlay = Some(r.translate(area.min.to_vec2()));
                    }
                }
            }
            if dr.drag_stopped() {
                // commit the snap on release, applying the hold-to-maximize escalation
                if let Some(p) = ui.ctx().pointer_latest_pos() {
                    let fx = (p.x - area.min.x) / asz.x;
                    let fy = (p.y - area.min.y) / asz.y;
                    let now = ui.input(|inp| inp.time);
                    let raw = detect_zone(fx, fy);
                    let held = if raw == self.dwell_zone {
                        now - self.dwell_start
                    } else {
                        0.0
                    };
                    let (committed, _) = resolve_zone(raw, held, asz);
                    if let Some(zone) = committed {
                        let w = &mut self.windows[i];
                        // remember the free-floating rect so restore/un-snap works
                        w.prev = Some(w.rect);
                        w.snap = Some(zone);
                        w.rect = zone_rect(zone, asz);
                        scr = w.rect.translate(area.min.to_vec2());
                    }
                }
                self.dwell_zone = None;
            }

            let title_rect = egui::Rect::from_min_size(scr.min, egui::vec2(scr.width(), TITLE_H));
            let content_rect = egui::Rect::from_min_max(
                egui::pos2(scr.min.x + 1.0, scr.min.y + TITLE_H),
                egui::pos2(scr.max.x - 1.0, scr.max.y - 1.0),
            );

            // --- paint window ---
            let p = ui.painter_at(scr.intersect(area));
            p.rect_filled(scr, egui::CornerRadius::same(6), WIN_BG);
            let (tbg, tbg_focus) = if is_project {
                (PROJ_TITLE_BG, PROJ_TITLE_BG_FOCUS)
            } else {
                (TITLE_BG, TITLE_BG_FOCUS)
            };
            p.rect_filled(
                title_rect,
                egui::CornerRadius::same(6),
                if is_focus { tbg_focus } else { tbg },
            );
            p.text(
                egui::pos2(scr.min.x + 11.0, scr.min.y + TITLE_H / 2.0),
                egui::Align2::LEFT_CENTER,
                &self.windows[i].title,
                egui::FontId::proportional(12.5),
                if is_focus { TEXT } else { DIM },
            );

            // --- window controls ---
            let by = scr.min.y + 3.0;
            let bh = TITLE_H - 6.0;
            let mut bx = scr.max.x - 4.0 - 22.0;
            for (role, glyph, danger) in
                [("close", "✕", true), ("max", "▢", false), ("min", "—", false)]
            {
                let r = egui::Rect::from_min_size(egui::pos2(bx, by), egui::vec2(22.0, bh));
                let resp = ui.interact(r, base.with((id, role)), egui::Sense::click());
                let bg = if resp.hovered() {
                    if danger {
                        egui::Color32::from_rgb(120, 45, 36)
                    } else {
                        egui::Color32::from_rgb(72, 64, 50)
                    }
                } else {
                    egui::Color32::TRANSPARENT
                };
                ui.painter().rect_filled(r, egui::CornerRadius::same(4), bg);
                ui.painter().text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    glyph,
                    egui::FontId::proportional(12.0),
                    if is_focus { TEXT } else { DIM },
                );
                if resp.clicked() {
                    acts.push(match role {
                        "close" => Act::Close(id),
                        "max" => Act::Max(id),
                        _ => Act::Min(id),
                    });
                }
                bx -= 25.0;
            }

            // --- content ---
            // Terminals need click_and_drag (for text selection); projects only
            // sense clicks so drags pass through to their own sub-windows.
            let sense = if is_project {
                egui::Sense::click()
            } else {
                egui::Sense::click_and_drag()
            };
            let cresp = ui.interact(content_rect, base.with((id, "content")), sense);
            if cresp.clicked() {
                acts.push(Act::Focus(id));
            }
            self.windows[i]
                .content
                .show(ui, content_rect, is_focus, base, id, &cresp);

            // --- border + resize ---
            ui.painter_at(area).rect_stroke(
                scr,
                egui::CornerRadius::same(6),
                egui::Stroke::new(1.0, if is_focus { BORDER_FOCUS } else { BORDER }),
                egui::StrokeKind::Inside,
            );
            let rh = egui::Rect::from_min_size(
                egui::pos2(scr.max.x - 15.0, scr.max.y - 15.0),
                egui::vec2(15.0, 15.0),
            );
            let rr = ui.interact(rh, base.with((id, "rsz")), egui::Sense::drag());
            if rr.dragged() {
                let w = &mut self.windows[i];
                let mut nr = w.rect;
                nr.max.x = (nr.max.x + rr.drag_delta().x).max(nr.min.x + 240.0);
                nr.max.y = (nr.max.y + rr.drag_delta().y).max(nr.min.y + 140.0);
                w.rect = nr;
                w.snap = None; // manual resize un-snaps
                clamp(&mut w.rect, asz);
            }
            ui.painter().line_segment(
                [egui::pos2(rh.min.x + 4.0, rh.max.y - 3.0), egui::pos2(rh.max.x - 3.0, rh.min.y + 4.0)],
                egui::Stroke::new(1.0, DIM),
            );
        }

        // --- snap overlay (amber), painted above all windows while dragging ---
        if let Some(ov) = snap_overlay {
            let p = ui.painter_at(area);
            let r = ov.intersect(area);
            p.rect_filled(r, egui::CornerRadius::same(8), SNAP_FILL);
            p.rect_stroke(
                r,
                egui::CornerRadius::same(8),
                egui::Stroke::new(1.5, SNAP_STROKE),
                egui::StrokeKind::Inside,
            );
        }

        // --- taskbar (minimized) ---
        let mins: Vec<(WinId, String)> = self
            .windows
            .iter()
            .filter(|w| w.minimized)
            .map(|w| (w.id, w.title.clone()))
            .collect();
        if !mins.is_empty() {
            let mut tx = area.min.x + 8.0;
            let ty = area.max.y - 30.0;
            for (id, title) in mins {
                let r = egui::Rect::from_min_size(egui::pos2(tx, ty), egui::vec2(160.0, 24.0));
                let resp = ui.interact(r, base.with((id, "task")), egui::Sense::click());
                ui.painter().rect_filled(
                    r,
                    egui::CornerRadius::same(5),
                    egui::Color32::from_rgb(42, 38, 29),
                );
                ui.painter().text(
                    egui::pos2(tx + 9.0, ty + 12.0),
                    egui::Align2::LEFT_CENTER,
                    &title,
                    egui::FontId::proportional(11.5),
                    DIM,
                );
                if resp.clicked() {
                    acts.push(Act::Restore(id));
                }
                tx += 168.0;
            }
        }

        for a in acts {
            match a {
                Act::Focus(id) => self.focus(id),
                Act::Close(id) => {
                    self.windows.retain(|w| w.id != id);
                    if self.focused == Some(id) {
                        self.focused = None;
                    }
                }
                Act::Min(id) => {
                    if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                        w.minimized = true;
                    }
                    if self.focused == Some(id) {
                        self.focused = None;
                    }
                }
                Act::Restore(id) => {
                    if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                        w.minimized = false;
                    }
                    self.focus(id);
                }
                Act::Max(id) => {
                    if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                        if w.snap == Some(Zone::Max) {
                            w.snap = None;
                            if let Some(pr) = w.prev.take() {
                                w.rect = pr;
                            }
                        } else {
                            w.prev = Some(w.rect);
                            w.snap = Some(Zone::Max);
                            w.rect = zone_rect(Zone::Max, asz);
                        }
                    }
                    self.focus(id);
                }
            }
        }
    }
}

fn clamp(rect: &mut egui::Rect, area: egui::Vec2) {
    let w = rect.width().min(area.x);
    let h = rect.height().min(area.y);
    let x = rect.min.x.clamp(0.0, (area.x - w).max(0.0));
    let y = rect.min.y.clamp(0.0, (area.y - h).max(0.0));
    *rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h));
}
