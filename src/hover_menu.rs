//! Grouped hover menu shared by project chrome and the task-manager panel.

use eframe::egui;

pub enum Entry<A> {
    Header(&'static str),
    Item {
        label: &'static str,
        hint: Option<String>,
        mark: bool,
        act: A,
    },
    Divider,
}

/// The builder runs only when the anchor's menu is open.
pub fn show<A: Clone>(
    ui: &mut egui::Ui,
    menu_id: egui::Id,
    anchor: egui::Rect,
    area: egui::Rect,
    build: impl FnOnce() -> Vec<Entry<A>>,
    align_right: bool,
    dismiss: bool,
) -> Option<A> {
    let open_id = menu_id.with("open");
    if dismiss {
        ui.ctx().data_mut(|d| d.insert_temp(open_id, false));
        return None;
    }
    let was_open = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(open_id))
        .unwrap_or(false);
    if !was_open && !ui.rect_contains_pointer(anchor) {
        return None;
    }
    let items = build();
    let th = crate::theme::live(ui.ctx());
    let font = egui::FontId::proportional(12.0);
    let small = egui::FontId::proportional(10.0);
    let pad = 10.0;
    let label_w = items
        .iter()
        .map(|entry| match entry {
            Entry::Header(label) | Entry::Item { label, .. } => {
                ui.painter()
                    .layout_no_wrap((*label).into(), font.clone(), th.text)
                    .size()
                    .x
            }
            Entry::Divider => 0.0,
        })
        .fold(0.0f32, f32::max);
    let hint_w = items
        .iter()
        .filter_map(|entry| match entry {
            Entry::Item { hint: Some(h), .. } => Some(
                ui.painter()
                    .layout_no_wrap(h.clone(), small.clone(), th.dim)
                    .size()
                    .x,
            ),
            _ => None,
        })
        .fold(0.0f32, f32::max);
    let w =
        (label_w + if hint_w > 0.0 { hint_w + 40.0 } else { 0.0 } + pad * 2.0 + 16.0).max(100.0);
    let height = |entry: &Entry<A>| match entry {
        Entry::Header(_) => 19.0,
        Entry::Item { .. } => 22.0,
        Entry::Divider => 10.0,
    };
    let panel_h: f32 = items.iter().map(height).sum::<f32>() + 8.0;
    let below = anchor.bottom() + 2.0;
    let oy = if below + panel_h > area.max.y {
        (anchor.top() - 2.0 - panel_h).max(area.min.y)
    } else {
        below
    };
    let ox = if align_right {
        (anchor.right() - w).max(area.min.x)
    } else {
        anchor.left().min(area.max.x - w).max(area.min.x)
    };
    let panel = egui::Rect::from_min_size(egui::pos2(ox, oy), egui::vec2(w, panel_h));
    let mut clicked = None;
    egui::Area::new(menu_id)
        .order(egui::Order::Foreground)
        .fixed_pos(panel.min)
        .constrain(false)
        .default_size(panel.size())
        .movable(false)
        .show(ui.ctx(), |mui| {
            let mp = mui.painter();
            mp.rect_filled(panel, egui::CornerRadius::same(4), th.bg);
            mp.rect_stroke(
                panel,
                egui::CornerRadius::same(4),
                egui::Stroke::new(1.0, th.border),
                egui::StrokeKind::Inside,
            );
            let mut y = panel.min.y + 4.0;
            for (ri, entry) in items.iter().enumerate() {
                let h = height(entry);
                let rr = egui::Rect::from_min_size(egui::pos2(panel.min.x, y), egui::vec2(w, h));
                match entry {
                    Entry::Header(label) => {
                        mui.painter().text(
                            egui::pos2(rr.min.x + pad, rr.center().y),
                            egui::Align2::LEFT_CENTER,
                            *label,
                            small.clone(),
                            th.dim,
                        );
                    }
                    Entry::Divider => {
                        mui.painter().line_segment(
                            [
                                egui::pos2(rr.min.x + pad, rr.center().y),
                                egui::pos2(rr.max.x - pad, rr.center().y),
                            ],
                            egui::Stroke::new(1.0, th.border),
                        );
                    }
                    Entry::Item {
                        label,
                        hint,
                        mark,
                        act,
                    } => {
                        let response =
                            mui.interact(rr, menu_id.with(("item", ri)), egui::Sense::click());
                        if response.hovered() {
                            mui.painter().rect_filled(rr, 0.0, th.sel_bg);
                        }
                        mui.painter().text(
                            egui::pos2(rr.min.x + pad, rr.center().y),
                            egui::Align2::LEFT_CENTER,
                            *label,
                            font.clone(),
                            th.text,
                        );
                        if *mark {
                            mui.painter().text(
                                egui::pos2(rr.min.x + pad + label_w + 4.0, rr.center().y),
                                egui::Align2::LEFT_CENTER,
                                "●",
                                small.clone(),
                                th.dim,
                            );
                        }
                        if let Some(hint) = hint {
                            mui.painter().text(
                                egui::pos2(rr.max.x - pad, rr.center().y),
                                egui::Align2::RIGHT_CENTER,
                                hint,
                                small.clone(),
                                th.dim,
                            );
                        }
                        if response.clicked() {
                            clicked = Some(act.clone());
                        }
                    }
                }
                y += h;
            }
        });
    let ptr_near = ui
        .ctx()
        .pointer_latest_pos()
        .is_some_and(|p| anchor.union(panel).expand(4.0).contains(p));
    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
    ui.ctx()
        .data_mut(|d| d.insert_temp(open_id, ptr_near && !esc && clicked.is_none()));
    clicked
}
