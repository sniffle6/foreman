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
    /// Choices side by side (the launcher's where-strip). A click returns
    /// its act and keeps the menu open. Too many to fit scroll sideways.
    Strip(Vec<Seg<A>>),
    Divider,
}

/// One choice in an [`Entry::Strip`].
pub struct Seg<A> {
    pub label: String,
    pub selected: bool,
    /// Drawn in a fixed slot before the label, so a state change never moves
    /// the strip. `None` reserves no slot.
    pub mark: SegMark,
    /// The label dimmed (an offline Remote); it stays clickable.
    pub dim: bool,
    pub tip: String,
    pub act: A,
}

/// A segment's status glyph. Each reads by shape, not only by colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegMark {
    None,
    /// Filled dot.
    Up,
    /// Hollow ring.
    Down,
    /// Small pip: an answer is on its way.
    Pending,
    /// Ring with a pip inside: the check cannot tell.
    Unsure,
}

/// Longest label a segment shows before it is cut with "…".
const SEG_TEXT_MAX: f32 = 96.0;
/// Text inset inside a segment.
const SEG_PAD: f32 = 6.0;
const SEG_GAP: f32 = 2.0;
/// The status glyph's slot, including its gap to the label.
const MARK_SLOT: f32 = 11.0;
/// Widest the strip may make the menu before it scrolls instead.
const STRIP_MAX_W: f32 = 280.0;
const STRIP_H: f32 = 28.0;

/// The menu's width: its rows' own, widened by a strip up to
/// [`STRIP_MAX_W`] (past it the strip scrolls), and never wider than the
/// space it opens in.
fn menu_width(rows_w: f32, strip_need: f32, area_w: f32) -> f32 {
    rows_w
        .max(strip_need.min(STRIP_MAX_W))
        .min(area_w.max(100.0))
}

fn seg_width<A>(s: &Seg<A>, text_w: f32) -> f32 {
    text_w.min(SEG_TEXT_MAX)
        + SEG_PAD * 2.0
        + if s.mark == SegMark::None {
            0.0
        } else {
            MARK_SLOT
        }
}

/// Paint `mark` centred at `c`, on device-pixel centres so a 1 px ring stays
/// one pixel wide at any scale.
fn paint_mark(
    p: &egui::Painter,
    ppp: f32,
    c: egui::Pos2,
    mark: SegMark,
    on: egui::Color32,
    off: egui::Color32,
) {
    let snap = |v: f32| ((v * ppp).floor() + 0.5) / ppp;
    let c = egui::pos2(snap(c.x), snap(c.y));
    // Whole device pixels: one at 100-150%, two at 200%.
    let ring = egui::Stroke::new(ppp.round().max(1.0) / ppp, off);
    match mark {
        SegMark::None => {}
        SegMark::Up => {
            p.circle_filled(c, 3.5, on);
        }
        SegMark::Down => {
            p.circle_stroke(c, 3.0, ring);
        }
        SegMark::Pending => {
            p.circle_filled(c, 1.5, off);
        }
        SegMark::Unsure => {
            p.circle_stroke(c, 3.0, ring);
            p.circle_filled(c, 1.0, off);
        }
    }
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
            Entry::Divider | Entry::Strip(_) => 0.0,
        })
        .fold(0.0f32, f32::max);
    // Each strip label's natural width, laid out once for sizing and drawing.
    let seg_text: Vec<Vec<f32>> = items
        .iter()
        .map(|entry| match entry {
            Entry::Strip(segs) => segs
                .iter()
                .map(|s| {
                    ui.painter()
                        .layout_no_wrap(s.label.clone(), font.clone(), th.text)
                        .size()
                        .x
                })
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    let strip_w = items
        .iter()
        .zip(&seg_text)
        .filter_map(|(entry, widths)| match entry {
            Entry::Strip(segs) => Some(
                segs.iter()
                    .zip(widths)
                    .map(|(s, w)| seg_width(s, *w) + SEG_GAP)
                    .sum::<f32>()
                    - SEG_GAP,
            ),
            _ => None,
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
    let rows_w =
        (label_w + if hint_w > 0.0 { hint_w + 40.0 } else { 0.0 } + pad * 2.0 + 16.0).max(100.0);
    let w = menu_width(rows_w, strip_w + (pad - SEG_PAD) * 2.0, area.width());
    let height = |entry: &Entry<A>| match entry {
        Entry::Header(_) => 19.0,
        Entry::Item { .. } => 22.0,
        Entry::Strip(_) => STRIP_H,
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
    let mut keep_open = false;
    let ppp = ui.ctx().pixels_per_point();
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
                    Entry::Strip(segs) => {
                        let inset = pad - SEG_PAD;
                        let strip = egui::Rect::from_min_max(
                            egui::pos2(rr.min.x + inset, rr.min.y + 2.0),
                            egui::pos2(rr.max.x - inset, rr.max.y - 2.0),
                        );
                        let mut sui = mui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(strip)
                                .layout(egui::Layout::left_to_right(egui::Align::Center)),
                        );
                        egui::ScrollArea::horizontal()
                            .id_salt(menu_id.with(("strip", ri)))
                            .auto_shrink([false, true])
                            .show(&mut sui, |sui| {
                                sui.spacing_mut().item_spacing.x = SEG_GAP;
                                for (si, seg) in segs.iter().enumerate() {
                                    let sw = seg_width(seg, seg_text[ri][si]);
                                    let (sr, resp) = sui.allocate_exact_size(
                                        egui::vec2(sw, strip.height()),
                                        egui::Sense::click(),
                                    );
                                    let resp = resp.on_hover_text(&seg.tip);
                                    let sp = sui.painter();
                                    let radius = egui::CornerRadius::same(4);
                                    if seg.selected {
                                        // Fill and outline: selected reads by
                                        // shape, not just a tint.
                                        sp.rect_filled(sr, radius, th.sel_bg);
                                        sp.rect_stroke(
                                            sr.shrink(0.5),
                                            radius,
                                            egui::Stroke::new(1.0, th.border_focus),
                                            egui::StrokeKind::Inside,
                                        );
                                        if !was_open {
                                            sui.scroll_to_rect(sr, None);
                                        }
                                    } else if resp.hovered() {
                                        sp.rect_filled(sr, radius, th.win_btn_hover);
                                    }
                                    let mut x = sr.min.x + SEG_PAD;
                                    if seg.mark != SegMark::None {
                                        paint_mark(
                                            sp,
                                            ppp,
                                            egui::pos2(x + 3.5, sr.center().y),
                                            seg.mark,
                                            th.border_focus,
                                            th.dim,
                                        );
                                        x += MARK_SLOT;
                                    }
                                    let color = if seg.dim { th.dim } else { th.text };
                                    let mut job = egui::text::LayoutJob::simple_singleline(
                                        seg.label.clone(),
                                        font.clone(),
                                        color,
                                    );
                                    job.wrap =
                                        egui::text::TextWrapping::truncate_at_width(SEG_TEXT_MAX);
                                    let galley = sp.layout_job(job);
                                    let at = egui::pos2(x, sr.center().y - galley.size().y / 2.0);
                                    sp.galley(at, galley, color);
                                    if resp.clicked() {
                                        clicked = Some(seg.act.clone());
                                        keep_open = true;
                                    }
                                }
                            });
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
    ui.ctx().data_mut(|d| {
        d.insert_temp(
            open_id,
            ptr_near && !esc && (clicked.is_none() || keep_open),
        )
    });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(label: &str, mark: SegMark) -> Seg<u8> {
        Seg {
            label: label.into(),
            selected: false,
            mark,
            dim: false,
            tip: String::new(),
            act: 0,
        }
    }

    #[test]
    fn a_strip_widens_the_menu_only_to_its_cap_and_never_past_the_area() {
        // Fits: the menu grows to hold it.
        assert_eq!(menu_width(150.0, 200.0, 1000.0), 200.0);
        // Too many names: capped, and the strip scrolls inside.
        assert_eq!(menu_width(150.0, 900.0, 1000.0), STRIP_MAX_W);
        // Rows wider than the strip keep their width.
        assert_eq!(menu_width(300.0, 120.0, 1000.0), 300.0);
        // A narrow window wins over both.
        assert_eq!(menu_width(150.0, 900.0, 180.0), 180.0);
    }

    #[test]
    fn a_long_name_is_cut_and_every_remote_reserves_its_status_slot() {
        let long = seg(&"x".repeat(200), SegMark::Pending);
        assert_eq!(
            seg_width(&long, 1000.0),
            SEG_TEXT_MAX + SEG_PAD * 2.0 + MARK_SLOT
        );
        // The slot is there before the first answer, so nothing moves when
        // it lands.
        for mark in [SegMark::Pending, SegMark::Up, SegMark::Down, SegMark::Unsure] {
            assert_eq!(seg_width(&seg("desk", mark), 30.0), 30.0 + SEG_PAD * 2.0 + MARK_SLOT);
        }
        assert_eq!(seg_width(&seg("Local", SegMark::None), 30.0), 30.0 + SEG_PAD * 2.0);
    }

    #[test]
    fn many_long_remotes_keep_the_menu_inside_its_area() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("menu");
        let area = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let anchor = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(16.0, 16.0));
        ctx.data_mut(|d| d.insert_temp(id.with("open"), true));
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(area),
                events: vec![egui::Event::PointerMoved(anchor.center())],
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                show(
                    ui,
                    id,
                    anchor,
                    area,
                    || {
                        let segs = (0..20)
                            .map(|i| seg(&format!("a-very-long-remote-name-{i}"), SegMark::Up))
                            .collect();
                        vec![
                            Entry::Strip(segs),
                            Entry::Item {
                                label: "Claude",
                                hint: None,
                                mark: false,
                                act: 1,
                            },
                        ]
                    },
                    false,
                    false,
                );
            });
        }
        let rect = ctx
            .memory(|m| m.area_rect(id))
            .expect("the menu drew");
        assert!(rect.width() <= STRIP_MAX_W + 0.5, "{rect:?}");
        assert!(area.contains_rect(rect), "{rect:?}");
    }
}
