//! Task-manager panel: plain-data model + shallow view.
//!
//! Read seam vocabulary (`TargetPath`, `PanelModel`) is built by
//! `WindowManager::panel_model()`. The view paints rows and records clicks into
//! fields drained after the draw pass (same deferred-Act pattern as chat).

use crate::theme::*;
use crate::wm::{Dir, WinId};
use eframe::egui;

/// Expanded panel width target (px).
pub const PANEL_W: f32 = 260.0;
/// Collapsed rail width (px).
pub const RAIL_W: f32 = 36.0;
/// Hard cap on expanded extent when side-docked (width), px.
pub const PANEL_MAX_SIDE: f32 = 420.0;
/// Hard cap on expanded extent when top/bottom-docked (height), px.
/// Header + ~8 rows — enough to browse sessions without crushing the landing
/// (or a sole project) when the panel is the only other leaf.
pub const PANEL_MAX_EDGE: f32 = 240.0;

/// Clamp for expanded panel extent along the dock axis.
/// Hard max depends on dock edge; also never more than half the available axis.
pub fn max_expanded(dock: Dir, axis_len: f32) -> f32 {
    let hard = match dock {
        Dir::Left | Dir::Right => PANEL_MAX_SIDE,
        Dir::Up | Dir::Down => PANEL_MAX_EDGE,
    };
    hard.min((axis_len * 0.5).max(RAIL_W)).max(RAIL_W + 40.0)
}
/// Per-project column width in horizontal (columns) mode (px).
const GROUP_W: f32 = 200.0;
/// Horizontal body shorter than this (project row + one tab row) falls back
/// to the single-line chip strip.
const STRIP_H: f32 = 48.0;
/// Chip label truncation budget in strip mode (px).
const CHIP_LABEL_W: f32 = 90.0;

/// Address of a row in the panel / `surface_target` write seam.
///
/// Semantics depend on the manager that interprets the path:
/// - **Desktop:** `project` is a desktop window id. When `window` is `None`,
///   optional `tab` selects a project tab on that window. When `window` is
///   `Some`, it is a child window inside a nested project manager, `ptab` is
///   the project tab owning it, and `tab` is the child's tab index.
/// - **Project manager (crew board):** `project` is a local window id, `window`
///   is `None`, and `tab` selects a tab on that window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TargetPath {
    pub project: WinId,
    /// Owning project-tab index on `project` when `window` is `Some`. Nested
    /// managers number child windows independently (each starts at 1), so
    /// tabbed projects collide under a bare child-id scan — this pins the tab.
    pub ptab: Option<usize>,
    pub window: Option<WinId>,
    pub tab: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowKind {
    Terminal(crate::icons::IconKind),
    Chat,
}

#[derive(Clone, Debug)]
pub struct TabEntry {
    pub path: TargetPath,
    pub title: String,
    pub kind: RowKind,
    /// The child *window* holding this tab is minimized inside its project.
    pub minimized: bool,
    /// This tab is its window's active tab (background tabs render dimmer).
    pub active_tab: bool,
    /// This tab is the focused leaf of the focused project.
    pub focused: bool,
    /// Terminal child process has exited (chat rows: always false).
    pub exited: bool,
    /// Terminal has a latched Bell (rang, not yet focused). Chat rows: false.
    pub bell: bool,
}

#[derive(Clone, Debug)]
pub struct ProjectEntry {
    pub path: TargetPath,
    pub title: String,
    pub minimized: bool,
    pub focused: bool,
    /// Any tab in this project has a latched Bell (drives the rail icons,
    /// where individual rows are not visible).
    pub bell: bool,
    pub tabs: Vec<TabEntry>,
}

#[derive(Clone, Debug, Default)]
pub struct PanelModel {
    pub projects: Vec<ProjectEntry>,
    /// Version string to show as the panel's update chip (None = hidden).
    pub update: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelBtn {
    Min,
    Close,
}

/// Per-window view state for the task-manager panel (shallow view).
/// `model` is stashed by the desktop each frame before the draw pass;
/// `click` / `hover_act` / `toggle_collapse` are drained after it.
pub struct PanelView {
    pub model: PanelModel,
    pub collapsed: bool,
    pub expanded_width: f32,
    /// Edge the panel is docked against (`Right` default). Updated from the
    /// live tree while the panel has a sibling; retained when it is the sole
    /// leaf (all projects minimized) so re-tile does not shove it back to the
    /// right rail. Only changes when the user moves the panel in the tree.
    pub dock: Dir,
    pub scroll: f32,
    pub click: Option<TargetPath>,
    pub hover_act: Option<(TargetPath, PanelBtn)>,
    pub toggle_collapse: bool,
    /// Latched when the user clicks the update chip; wm drains it each frame.
    pub update_click: bool,
}

impl PanelView {
    pub fn new(collapsed: bool, expanded_width: f32) -> Self {
        Self::with_dock(collapsed, expanded_width, Dir::Right)
    }

    pub fn with_dock(collapsed: bool, expanded_width: f32, dock: Dir) -> Self {
        let max = max_expanded(dock, 10_000.0); // no area yet; hard cap only
        Self {
            model: PanelModel::default(),
            collapsed,
            expanded_width: expanded_width.clamp(RAIL_W + 40.0, max),
            dock,
            scroll: 0.0,
            click: None,
            hover_act: None,
            toggle_collapse: false,
            update_click: false,
        }
    }

    /// Paint the panel body (below the window title band). Records row
    /// interactions into `click` / `hover_act` / scroll; does not mutate the tree.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: egui::Rect, base: egui::Id) {
        // Bell: the panel may be the only visible surface for a ringing
        // session (minimized window, collapsed rail), so it drives its own
        // breathe repaint. Gated here like every other Bell paint site.
        let bell_gate = crate::terminal::bell_enabled(ui.ctx());
        if bell_gate && self.model.projects.iter().any(|pr| pr.bell) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(30));
        }
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, BG);

        // Quiet update-available chip pinned to the bottom edge of every
        // expanded layout (rows, columns, strip); the collapsed rails get a
        // lone ↓ glyph instead (paint_rail / paint_rail_h). Shrinks `body` so
        // row layout never overlaps the chip — every expanded painter below
        // must take `body`, not `rect`.
        const UPDATE_CHIP_H: f32 = 26.0;
        let mut body = rect;
        if !self.collapsed && self.model.update.is_some() {
            let footer = egui::Rect::from_min_max(
                egui::pos2(rect.min.x, rect.max.y - UPDATE_CHIP_H),
                rect.max,
            );
            body.max.y -= UPDATE_CHIP_H;
            let ver = self.model.update.clone().unwrap();
            self.paint_update_chip(ui, footer, base, &ver);
        }

        // Flow follows the rect the leaf was given this frame: wider than tall
        // means the panel is bottom/top-docked and content runs left-to-right.
        // No stored state — move the leaf back to a tall slot and it flips back.
        let horizontal = rect.width() > rect.height();
        if self.collapsed {
            if horizontal {
                self.paint_rail_h(ui, rect, base);
            } else {
                self.paint_rail(ui, rect, base);
            }
            return;
        }
        if horizontal {
            if rect.height() < STRIP_H {
                self.paint_strip(ui, body, base);
            } else {
                self.paint_columns(ui, body, base);
            }
            return;
        }

        let row_h = 22.0;
        // Entry clamp: `scroll` may carry a large x-offset back from a wide
        // horizontal dock; without it every row paints above the rect (panel
        // looks empty) until the first wheel tick re-clamps.
        let content_h: f32 = 4.0
            + self
                .model
                .projects
                .iter()
                .map(|p| row_h * (1.0 + p.tabs.len() as f32) + 4.0)
                .sum::<f32>();
        self.scroll = self.scroll.clamp(0.0, (content_h - body.height()).max(0.0));
        let mut y = body.min.y + 4.0 - self.scroll;
        let start_y = y;

        // Collect paint specs first so we can mutate self (click/hover) without
        // holding a borrow on model.
        let mut specs: Vec<(egui::Rect, egui::Id, RowPaintOwned)> = Vec::new();
        for (pi, proj) in self.model.projects.iter().enumerate() {
            let row = egui::Rect::from_min_size(
                egui::pos2(body.min.x + 4.0, y),
                egui::vec2((body.width() - 8.0).max(0.0), row_h),
            );
            if row_visible(row, body) {
                specs.push((
                    row,
                    base.with(("prow", pi, proj.path.project)),
                    RowPaintOwned {
                        path: proj.path,
                        title: proj.title.clone(),
                        kind: None,
                        focused: proj.focused,
                        minimized: proj.minimized,
                        background_tab: false,
                        exited: false,
                        bell: false,
                        project_row: true,
                    },
                ));
            }
            y += row_h;

            for (ti, t) in proj.tabs.iter().enumerate() {
                let row = egui::Rect::from_min_size(
                    egui::pos2(body.min.x + 16.0, y),
                    egui::vec2((body.width() - 20.0).max(0.0), row_h),
                );
                if row_visible(row, body) {
                    specs.push((
                        row,
                        base.with(("trow", pi, ti, t.path.window, t.path.tab)),
                        RowPaintOwned {
                            path: t.path,
                            title: t.title.clone(),
                            kind: Some(t.kind),
                            focused: t.focused,
                            minimized: t.minimized,
                            background_tab: !t.active_tab,
                            exited: t.exited,
                            bell: bell_gate && t.bell,
                            project_row: false,
                        },
                    ));
                }
                y += row_h;
            }
            y += 4.0;
        }
        for (row, id, rp) in specs {
            self.paint_row(ui, row, id, body, rp);
        }

        let content_h = (y - start_y) + self.scroll;
        let resp = ui.interact(body, base.with("panel-scroll"), egui::Sense::hover());
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if resp.hovered() && scroll != 0.0 {
            let max_scroll = (content_h - body.height()).max(0.0);
            self.scroll = (self.scroll - scroll).clamp(0.0, max_scroll);
        }
    }

    /// Quiet update-available chip pinned to the panel's bottom edge.
    /// Click is recorded (deferred-Act, like `self.click`) and drained by wm.
    fn paint_update_chip(
        &mut self,
        ui: &mut egui::Ui,
        footer: egui::Rect,
        base: egui::Id,
        ver: &str,
    ) {
        let p = ui.painter();
        let chip = footer.shrink2(egui::vec2(6.0, 3.0));
        let id = base.with("update-chip");
        let resp = ui.interact(chip, id, egui::Sense::click());
        if resp.hovered() {
            p.rect_filled(chip, egui::CornerRadius::same(5), SEL_BG);
        }
        let text = format!("↓ {ver} — click for release notes");
        let col = if resp.hovered() { SNAP_STROKE } else { DIM };
        let galley = p.layout_no_wrap(text, egui::FontId::proportional(12.0), col);
        let pos = egui::pos2(chip.min.x + 6.0, chip.center().y - galley.size().y / 2.0);
        p.galley(pos, galley, col);
        if resp.clicked() {
            self.update_click = true;
        }
    }

    /// Collapsed-rail counterpart of the footer chip: a lone ↓ glyph carrying
    /// the same id, tooltip, and `update_click` latch, so an update is
    /// noticeable without expanding the panel. Fills BG behind itself because
    /// the vertical rail has no scroll clamp and icons can flow underneath.
    fn paint_rail_update_glyph(
        &mut self,
        ui: &mut egui::Ui,
        cell: egui::Rect,
        base: egui::Id,
        ver: &str,
    ) {
        let p = ui.painter();
        p.rect_filled(cell, egui::CornerRadius::same(5), BG);
        let resp = ui
            .interact(cell, base.with("update-chip"), egui::Sense::click())
            .on_hover_text(format!("{ver} — click for release notes"));
        if resp.hovered() {
            p.rect_filled(cell, egui::CornerRadius::same(5), SEL_BG);
        }
        let col = if resp.hovered() { TEXT } else { SNAP_STROKE };
        let galley = p.layout_no_wrap("↓".into(), egui::FontId::proportional(14.0), col);
        p.galley(
            egui::pos2(
                cell.center().x - galley.size().x / 2.0,
                cell.center().y - galley.size().y / 2.0,
            ),
            galley,
            col,
        );
        if resp.clicked() {
            self.update_click = true;
        }
    }

    fn paint_rail(&mut self, ui: &mut egui::Ui, rect: egui::Rect, base: egui::Id) {
        let bell_gate = crate::terminal::bell_enabled(ui.ctx());
        let p = ui.painter().with_clip_rect(rect);
        let rails: Vec<_> = self
            .model
            .projects
            .iter()
            .enumerate()
            .map(|(pi, proj)| {
                (
                    pi,
                    proj.path,
                    proj.title.clone(),
                    proj.focused,
                    proj.minimized,
                    proj.bell,
                )
            })
            .collect();
        let mut y = rect.min.y + 6.0;
        for (pi, path, title, focused, minimized, bell) in rails {
            let cell = egui::Rect::from_center_size(
                egui::pos2(rect.center().x, y + 14.0),
                egui::vec2(28.0, 28.0),
            );
            let resp = ui
                .interact(
                    cell,
                    base.with(("rail", pi, path.project)),
                    egui::Sense::click(),
                )
                .on_hover_text(&title);
            if focused || resp.hovered() {
                p.rect_filled(cell, egui::CornerRadius::same(5), SEL_BG);
            }
            if focused {
                p.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(rect.min.x + 2.0, cell.min.y + 6.0),
                        egui::vec2(2.0, cell.height() - 12.0),
                    ),
                    0.0,
                    BORDER_FOCUS,
                );
            }
            let tint = if minimized {
                DIM
            } else {
                crate::icons::IconKind::Folder.tint()
            };
            paint_icon(
                ui,
                &p,
                cell.center(),
                14.0,
                crate::icons::IconKind::Folder,
                tint,
            );
            // Collapsed rail is the only surface for this project's rows — a
            // ringing child shows as a pulsing dot on the project icon.
            if bell_gate && bell {
                p.circle_filled(
                    egui::pos2(cell.max.x - 3.0, cell.min.y + 3.0),
                    3.0,
                    bell_pulse(ui.input(|i| i.time)),
                );
            }
            if resp.clicked() {
                self.click = Some(path);
            }
            y += 32.0;
        }
        if let Some(ver) = self.model.update.clone() {
            let cell = egui::Rect::from_center_size(
                egui::pos2(rect.center().x, rect.max.y - 16.0),
                egui::vec2(22.0, 22.0),
            );
            self.paint_rail_update_glyph(ui, cell, base, &ver);
        }
    }

    /// Horizontal columns mode: one fixed-width group per project (project row
    /// on top, its tab rows below), groups flowing left-to-right with a
    /// hairline between them. Rows reuse `paint_row` — only the cursor advance
    /// changes (y within a group, x between groups) and the truncation budget
    /// comes from the group width. Scroll is horizontal only; rows past the
    /// group height are clipped.
    fn paint_columns(&mut self, ui: &mut egui::Ui, rect: egui::Rect, base: egui::Id) {
        let bell_gate = crate::terminal::bell_enabled(ui.ctx());
        let row_h = 22.0;
        let gap = 9.0; // pad + hairline + pad between groups
        let n = self.model.projects.len();
        let content_w = n as f32 * GROUP_W + n.saturating_sub(1) as f32 * gap + 8.0;
        let max_scroll = (content_w - rect.width()).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);

        let mut specs: Vec<(egui::Rect, egui::Rect, egui::Id, RowPaintOwned)> = Vec::new();
        let mut dividers: Vec<f32> = Vec::new();
        let mut x = rect.min.x + 4.0 - self.scroll;
        for (pi, proj) in self.model.projects.iter().enumerate() {
            let group = egui::Rect::from_min_size(
                egui::pos2(x, rect.min.y),
                egui::vec2(GROUP_W, rect.height()),
            );
            if pi + 1 < n {
                dividers.push(group.max.x + gap * 0.5);
            }
            x += GROUP_W + gap;
            if group.max.x < rect.min.x || group.min.x > rect.max.x {
                continue; // scrolled out of view: no paint, no interact
            }
            let clip = group.intersect(rect);
            let mut y = rect.min.y + 4.0;
            let row =
                egui::Rect::from_min_size(egui::pos2(group.min.x, y), egui::vec2(GROUP_W, row_h));
            specs.push((
                row,
                clip,
                base.with(("prow", pi, proj.path.project)),
                RowPaintOwned {
                    path: proj.path,
                    title: proj.title.clone(),
                    kind: None,
                    focused: proj.focused,
                    minimized: proj.minimized,
                    background_tab: false,
                    exited: false,
                    bell: false,
                    project_row: true,
                },
            ));
            y += row_h;
            for (ti, t) in proj.tabs.iter().enumerate() {
                if y >= rect.max.y {
                    break; // below the panel: clipped, never interactive
                }
                let row = egui::Rect::from_min_size(
                    egui::pos2(group.min.x + 12.0, y),
                    egui::vec2(GROUP_W - 12.0, row_h),
                );
                specs.push((
                    row,
                    clip,
                    base.with(("trow", pi, ti, t.path.window, t.path.tab)),
                    RowPaintOwned {
                        path: t.path,
                        title: t.title.clone(),
                        kind: Some(t.kind),
                        focused: t.focused,
                        minimized: t.minimized,
                        background_tab: !t.active_tab,
                        exited: t.exited,
                        bell: bell_gate && t.bell,
                        project_row: false,
                    },
                ));
                y += row_h;
            }
        }
        for (row, clip, id, rp) in specs {
            self.paint_row(ui, row, id, clip, rp);
        }
        let p = ui.painter_at(rect);
        for dx in dividers {
            p.line_segment(
                [
                    egui::pos2(dx, rect.min.y + 4.0),
                    egui::pos2(dx, rect.max.y - 4.0),
                ],
                egui::Stroke::new(1.0, BORDER.gamma_multiply(0.6)),
            );
        }
        self.hscroll(ui, rect, base, max_scroll);
    }

    /// Strip mode: the body is too short for stacked rows, so projects and
    /// their tabs flow as one line of chips with a hairline divider between
    /// projects. Click = surface; no hover min/close here — management means
    /// expanding the panel first.
    fn paint_strip(&mut self, ui: &mut egui::Ui, rect: egui::Rect, base: egui::Id) {
        struct Chip {
            id: egui::Id,
            path: TargetPath,
            galley: std::sync::Arc<egui::Galley>,
            kind: Option<RowKind>, // None = project chip
            focused: bool,
            dim: bool,
            bell: bool,
            div_before: bool,
            w: f32,
        }
        let bell_gate = crate::terminal::bell_enabled(ui.ctx());
        let p = ui.painter_at(rect);
        let chip_h = 24.0;
        let pad = 10.0;
        let icon_w = 14.0;
        let text_gap = 6.0;
        let chip_gap = 4.0;

        // Pass 1: measure. Galleys use the placeholder color so hover can
        // recolor them at paint time.
        let layout = |title: &str| {
            let mut job = egui::text::LayoutJob::simple_singleline(
                title.to_string(),
                egui::FontId::proportional(12.0),
                egui::Color32::PLACEHOLDER,
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(CHIP_LABEL_W);
            p.layout_job(job)
        };
        let mut chips: Vec<Chip> = Vec::new();
        for (pi, proj) in self.model.projects.iter().enumerate() {
            let galley = layout(&proj.title);
            chips.push(Chip {
                id: base.with(("pchip", pi, proj.path.project)),
                path: proj.path,
                w: pad + icon_w + text_gap + galley.size().x + pad,
                galley,
                kind: None,
                focused: proj.focused,
                dim: proj.minimized,
                bell: false, // the tab chips beside it carry the ring
                div_before: pi > 0,
            });
            for (ti, t) in proj.tabs.iter().enumerate() {
                let galley = layout(&t.title);
                chips.push(Chip {
                    id: base.with(("tchip", pi, ti, t.path.window, t.path.tab)),
                    path: t.path,
                    w: pad + icon_w + text_gap + galley.size().x + pad,
                    galley,
                    kind: Some(t.kind),
                    focused: t.focused,
                    dim: t.minimized || !t.active_tab || t.exited,
                    bell: bell_gate && t.bell,
                    div_before: false,
                });
            }
        }
        let content_w: f32 = 16.0
            + chips
                .iter()
                .map(|c| c.w + chip_gap + if c.div_before { 9.0 } else { 0.0 })
                .sum::<f32>();
        let max_scroll = (content_w - rect.width()).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);

        // Pass 2: place, interact, paint.
        let cy = rect.center().y;
        let mut x = rect.min.x + 8.0 - self.scroll;
        for chip in chips {
            if chip.div_before {
                x += 4.0;
                p.line_segment(
                    [egui::pos2(x, cy - 8.0), egui::pos2(x, cy + 8.0)],
                    egui::Stroke::new(1.0, BORDER.gamma_multiply(0.8)),
                );
                x += 5.0;
            }
            let chip_rect = egui::Rect::from_min_size(
                egui::pos2(x, cy - chip_h / 2.0),
                egui::vec2(chip.w, chip_h),
            );
            x += chip.w + chip_gap;
            if chip_rect.max.x < rect.min.x || chip_rect.min.x > rect.max.x {
                continue; // scrolled out of view: no paint, no interact
            }
            let resp = ui.interact(chip_rect, chip.id, egui::Sense::click());
            let over = resp.hovered() || resp.contains_pointer();
            if chip.focused || over {
                p.rect_filled(chip_rect, egui::CornerRadius::same(5), SEL_BG);
            }
            if chip.focused {
                p.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(chip_rect.min.x, chip_rect.min.y + 4.0),
                        egui::vec2(2.0, chip_h - 8.0),
                    ),
                    0.0,
                    BORDER_FOCUS,
                );
            }
            let col = if over || chip.focused {
                TEXT
            } else if chip.dim {
                DIM
            } else if chip.kind.is_none() {
                TEXT // project chips read brighter, like project rows
            } else {
                DIM
            };
            let icon_c = egui::pos2(chip_rect.min.x + pad + icon_w / 2.0, cy);
            match chip.kind {
                None => {
                    let tint = if chip.dim {
                        DIM
                    } else {
                        crate::icons::IconKind::Folder.tint()
                    };
                    paint_icon(ui, &p, icon_c, 12.0, crate::icons::IconKind::Folder, tint);
                }
                Some(RowKind::Terminal(k)) => {
                    let tint = if chip.dim { DIM } else { k.tint() };
                    paint_icon(ui, &p, icon_c, 12.0, k, tint);
                }
                Some(RowKind::Chat) => {
                    p.text(
                        icon_c,
                        egui::Align2::CENTER_CENTER,
                        "§",
                        egui::FontId::proportional(12.0),
                        col,
                    );
                }
            }
            let tp = egui::pos2(
                chip_rect.min.x + pad + icon_w + text_gap,
                cy - chip.galley.size().y / 2.0,
            );
            p.galley(tp, chip.galley, col);
            // Pulsing bell dot on the ringing terminal's chip corner.
            if chip.bell {
                p.circle_filled(
                    egui::pos2(chip_rect.max.x - 4.0, chip_rect.min.y + 4.0),
                    3.0,
                    bell_pulse(ui.input(|i| i.time)),
                );
            }
            if resp.clicked() {
                self.click = Some(chip.path);
            }
        }
        self.hscroll(ui, rect, base, max_scroll);
    }

    /// Collapsed horizontal rail: a 36px-tall strip with project icons flowing
    /// left-to-right and the expand toggle riding the right end *inside* the
    /// strip — the wm suppresses the header band entirely for a collapsed
    /// horizontal panel, so this toggle is the only mouse path back out.
    fn paint_rail_h(&mut self, ui: &mut egui::Ui, rect: egui::Rect, base: egui::Id) {
        // The expand glyph points along the grow axis: a bottom-docked panel
        // expands upward (⌃), a top-docked one downward (⌄). Same side test as
        // the expanded header's collapse glyph in wm.rs, mirrored.
        let up = rect.center().y >= ui.max_rect().center().y;
        let p = ui.painter_at(rect);
        // Icons clip against the reserved expand-toggle zone at the right end
        // (widened when the update glyph rides beside it).
        let update_ver = self.model.update.clone();
        let icons_max_x = rect.max.x - 28.0 - if update_ver.is_some() { 26.0 } else { 0.0 };
        let ip = ui.painter_at(egui::Rect::from_min_max(
            rect.min,
            egui::pos2(icons_max_x, rect.max.y),
        ));
        let bell_gate = crate::terminal::bell_enabled(ui.ctx());
        let rails: Vec<_> = self
            .model
            .projects
            .iter()
            .enumerate()
            .map(|(pi, proj)| {
                (
                    pi,
                    proj.path,
                    proj.title.clone(),
                    proj.focused,
                    proj.minimized,
                    proj.bell,
                )
            })
            .collect();
        let content_w = rails.len() as f32 * 32.0 + 12.0;
        let max_scroll = (content_w - (icons_max_x - rect.min.x)).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        let mut x = rect.min.x + 6.0 - self.scroll;
        for (pi, path, title, focused, minimized, bell) in rails {
            let cell = egui::Rect::from_center_size(
                egui::pos2(x + 14.0, rect.center().y),
                egui::vec2(28.0, 28.0),
            );
            x += 32.0;
            if cell.max.x < rect.min.x || cell.min.x > icons_max_x {
                continue; // out of view / under the expand toggle
            }
            let resp = ui
                .interact(
                    cell,
                    base.with(("rail", pi, path.project)),
                    egui::Sense::click(),
                )
                .on_hover_text(&title);
            if focused || resp.hovered() {
                ip.rect_filled(cell, egui::CornerRadius::same(5), SEL_BG);
            }
            if focused {
                // Horizontal rail: the focus stripe is an underline.
                ip.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(cell.min.x + 4.0, cell.max.y - 4.0),
                        egui::vec2(cell.width() - 8.0, 2.0),
                    ),
                    0.0,
                    BORDER_FOCUS,
                );
            }
            let tint = if minimized {
                DIM
            } else {
                crate::icons::IconKind::Folder.tint()
            };
            paint_icon(
                ui,
                &ip,
                cell.center(),
                14.0,
                crate::icons::IconKind::Folder,
                tint,
            );
            // Same pulsing-dot rule as the vertical rail.
            if bell_gate && bell {
                ip.circle_filled(
                    egui::pos2(cell.max.x - 3.0, cell.min.y + 3.0),
                    3.0,
                    bell_pulse(ui.input(|i| i.time)),
                );
            }
            if resp.clicked() {
                self.click = Some(path);
            }
        }
        let br = egui::Rect::from_center_size(
            egui::pos2(rect.max.x - 14.0, rect.center().y),
            egui::vec2(22.0, 22.0),
        );
        let resp = ui.interact(br, base.with("rail-expand"), egui::Sense::click());
        if resp.hovered() {
            p.rect_filled(br, egui::CornerRadius::same(4), SEL_BG);
        }
        paint_chevron(&p, br.center(), up, if resp.hovered() { TEXT } else { DIM });
        if resp.clicked() {
            self.toggle_collapse = true;
        }
        if let Some(ver) = update_ver {
            let cell = egui::Rect::from_center_size(
                egui::pos2(rect.max.x - 40.0, rect.center().y),
                egui::vec2(22.0, 22.0),
            );
            self.paint_rail_update_glyph(ui, cell, base, &ver);
        }
        self.hscroll(ui, rect, base, max_scroll);
    }

    /// Wheel → x-offset for the horizontal modes. Mouse wheels have no x axis,
    /// so the vertical delta drives the horizontal scroll; `.x` is added for
    /// trackpads that do emit it.
    fn hscroll(&mut self, ui: &mut egui::Ui, rect: egui::Rect, base: egui::Id, max_scroll: f32) {
        let resp = ui.interact(rect, base.with("panel-scroll"), egui::Sense::hover());
        let d = ui.input(|i| i.smooth_scroll_delta);
        let scroll = d.x + d.y;
        if resp.hovered() && scroll != 0.0 {
            self.scroll = (self.scroll - scroll).clamp(0.0, max_scroll);
        }
    }

    fn paint_row(
        &mut self,
        ui: &mut egui::Ui,
        row: egui::Rect,
        id: egui::Id,
        clip: egui::Rect,
        rp: RowPaintOwned,
    ) {
        let p = ui.painter().with_clip_rect(clip);
        let resp = ui.interact(row, id, egui::Sense::click());
        // Geometric containment, not `hovered()`: the min/close buttons below
        // are registered on top of this row, so hovering them un-hovers the
        // row — gating on `hovered()` made the buttons flicker in and out of
        // existence and drop their clicks (they only existed on frames the
        // row itself was topmost).
        let over = resp.hovered() || resp.contains_pointer();
        if rp.focused || over {
            p.rect_filled(row, egui::CornerRadius::same(4), SEL_BG);
        }
        if rp.focused {
            p.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(row.min.x, row.min.y + 4.0),
                    egui::vec2(2.0, row.height() - 8.0),
                ),
                0.0,
                BORDER_FOCUS,
            );
        }

        let col = if rp.exited {
            DIM
        } else if rp.focused {
            TEXT
        } else if rp.minimized || rp.background_tab {
            DIM
        } else {
            TEXT
        };

        // Icon
        let icon_c = egui::pos2(row.min.x + 9.0, row.center().y);
        match rp.kind {
            None => {
                let tint = if rp.minimized {
                    DIM
                } else {
                    crate::icons::IconKind::Folder.tint()
                };
                paint_icon(ui, &p, icon_c, 12.0, crate::icons::IconKind::Folder, tint);
            }
            Some(RowKind::Terminal(k)) => {
                let tint = if rp.minimized || rp.background_tab || rp.exited {
                    DIM
                } else {
                    k.tint()
                };
                paint_icon(ui, &p, icon_c, 12.0, k, tint);
            }
            Some(RowKind::Chat) => {
                p.text(
                    icon_c,
                    egui::Align2::CENTER_CENTER,
                    "§",
                    egui::FontId::proportional(12.0),
                    col,
                );
            }
        }

        // Title, truncated with … to the space left of the buttons/labels.
        let font = egui::FontId::proportional(if rp.project_row { 12.5 } else { 12.0 });
        let text_x = row.min.x + 20.0;
        let reserve = if over {
            38.0 // min + close buttons
        } else if rp.bell {
            20.0 // pulsing bell dot
        } else if rp.minimized || rp.background_tab {
            26.0 // "min"/"tab" label
        } else {
            6.0
        };
        let avail = (row.max.x - reserve - text_x).max(0.0);
        let mut job = egui::text::LayoutJob::simple_singleline(rp.title.clone(), font, col);
        job.wrap = egui::text::TextWrapping::truncate_at_width(avail);
        let galley = p.layout_job(job);
        let text_size = galley.size();
        p.galley(
            egui::pos2(text_x, row.center().y - text_size.y / 2.0),
            galley,
            col,
        );
        if rp.exited {
            let y = row.center().y;
            p.line_segment(
                [egui::pos2(text_x, y), egui::pos2(text_x + text_size.x, y)],
                egui::Stroke::new(1.0, DIM),
            );
        }

        let mut btn_hit = false;
        if over {
            let btn_y = row.center().y;
            let min_c = egui::pos2(row.max.x - 26.0, btn_y);
            let close_c = egui::pos2(row.max.x - 10.0, btn_y);
            let min_r = egui::Rect::from_center_size(min_c, egui::vec2(16.0, 16.0));
            let close_r = egui::Rect::from_center_size(close_c, egui::vec2(16.0, 16.0));
            let min_resp = ui.interact(min_r, id.with("min"), egui::Sense::click());
            let close_resp = ui.interact(close_r, id.with("close"), egui::Sense::click());
            p.text(
                min_c,
                egui::Align2::CENTER_CENTER,
                if rp.minimized { "□" } else { "–" },
                egui::FontId::proportional(11.0),
                if min_resp.hovered() { TEXT } else { DIM },
            );
            if close_resp.hovered() {
                p.rect_filled(close_r, egui::CornerRadius::same(3), DANGER);
            }
            p.text(
                close_c,
                egui::Align2::CENTER_CENTER,
                "×",
                egui::FontId::proportional(12.0),
                if close_resp.hovered() { TEXT } else { DIM },
            );
            if min_resp.clicked() {
                if rp.minimized {
                    // □ on a minimized row restores; MinPath would re-minimize.
                    self.click = Some(rp.path);
                } else {
                    self.hover_act = Some((rp.path, PanelBtn::Min));
                }
                btn_hit = true;
            }
            if close_resp.clicked() {
                self.hover_act = Some((rp.path, PanelBtn::Close));
                btn_hit = true;
            }
        } else if rp.bell {
            // Latched Bell: pulsing amber dot in the right-edge slot — the
            // attention cue outranks the "min"/"tab" state labels.
            p.circle_filled(
                egui::pos2(row.max.x - 12.0, row.center().y),
                3.5,
                bell_pulse(ui.input(|i| i.time)),
            );
        } else if rp.minimized {
            p.text(
                egui::pos2(row.max.x - 8.0, row.center().y),
                egui::Align2::RIGHT_CENTER,
                "min",
                egui::FontId::proportional(10.0),
                DIM,
            );
        } else if rp.background_tab {
            p.text(
                egui::pos2(row.max.x - 8.0, row.center().y),
                egui::Align2::RIGHT_CENTER,
                "tab",
                egui::FontId::proportional(10.0),
                DIM,
            );
        }

        if resp.clicked() && !btn_hit {
            self.click = Some(rp.path);
        }
    }
}

struct RowPaintOwned {
    path: TargetPath,
    title: String,
    kind: Option<RowKind>,
    focused: bool,
    minimized: bool,
    background_tab: bool,
    exited: bool,
    /// Latched Bell on this terminal row (already gated by the master switch).
    bell: bool,
    project_row: bool,
}

/// An up/down chevron as two line segments. The default egui fonts have no
/// glyph for U+2303/U+2304 (they render as tofu), so the expand/collapse
/// arrows are drawn as vector strokes — same policy as the wm control icons.
pub(crate) fn paint_chevron(p: &egui::Painter, c: egui::Pos2, up: bool, color: egui::Color32) {
    let (hw, hh) = (4.0, 2.2);
    let dy = if up { hh } else { -hh };
    let stroke = egui::Stroke::new(1.4, color);
    p.line_segment(
        [egui::pos2(c.x - hw, c.y + dy), egui::pos2(c.x, c.y - dy)],
        stroke,
    );
    p.line_segment(
        [egui::pos2(c.x, c.y - dy), egui::pos2(c.x + hw, c.y + dy)],
        stroke,
    );
}

fn row_visible(row: egui::Rect, clip: egui::Rect) -> bool {
    row.max.y >= clip.min.y && row.min.y <= clip.max.y
}

fn paint_icon(
    ui: &mut egui::Ui,
    p: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    kind: crate::icons::IconKind,
    tint: egui::Color32,
) {
    let rect = egui::Rect::from_center_size(center, egui::vec2(size, size));
    let px = (size * ui.ctx().pixels_per_point()).round().max(1.0) as u32;
    let tex = crate::icons::texture(ui.ctx(), kind, px);
    p.image(
        tex.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        tint,
    );
}
