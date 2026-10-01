//! A row of icon buttons that folds what doesn't fit behind a ">" menu, like
//! the JetBrains tool-window toolbars. Icons are painted from line segments,
//! so they scale with view zoom and need no font glyphs.

use eframe::egui;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Glyph {
    Refresh,
    Push,
    Add,
    Directories,
    Expand,
    Collapse,
    /// The ">" that opens the overflow menu.
    Chevron,
    /// Three dots: the overflow menu where a ">" would look odd.
    Dots,
}

/// One toolbar button. `label` is its name in the overflow menu, `hint` its
/// tooltip. `on`: a toggle that is currently on.
pub(super) struct Tool {
    pub glyph: Glyph,
    pub label: String,
    pub hint: String,
    pub enabled: bool,
    pub on: bool,
}

/// How many of `n` buttons fit in `avail`; when they don't all, the last slot
/// goes to the ">" button.
fn visible(n: usize, avail: f32, side: f32, gap: f32) -> usize {
    let width = |k: usize| k as f32 * side + k.saturating_sub(1) as f32 * gap;
    if width(n) <= avail {
        return n;
    }
    (0..n).rev().find(|&k| width(k + 1) <= avail).unwrap_or(0)
}

/// Draw `tools`; returns the index of the one clicked, from the row or from
/// the overflow menu.
pub(super) fn show(ui: &mut egui::Ui, tools: &[Tool], scale: f32) -> Option<usize> {
    let side = 22.0 * scale;
    let gap = 2.0 * scale;
    let shown = visible(tools.len(), ui.available_width(), side, gap);
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (i, tool) in tools.iter().take(shown).enumerate() {
            if button(ui, tool.glyph, side, tool.enabled, tool.on, &tool.hint).clicked() {
                clicked = Some(i);
            }
        }
        if shown < tools.len() {
            let more = button(ui, Glyph::Chevron, side, true, false, "More actions");
            if let Some(i) = overflow(&more, &tools[shown..]) {
                clicked = Some(shown + i);
            }
        }
    });
    clicked
}

/// The menu behind a button: `tools` as labeled rows. Returns the clicked one.
pub(super) fn overflow(anchor: &egui::Response, tools: &[Tool]) -> Option<usize> {
    let mut clicked = None;
    egui::Popup::menu(anchor)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            for (i, tool) in tools.iter().enumerate() {
                let item = ui.add_enabled(
                    tool.enabled,
                    egui::Button::new(&tool.label).selected(tool.on),
                );
                let item = item
                    .on_hover_text(&tool.hint)
                    .on_disabled_hover_text(&tool.hint);
                if item.clicked() {
                    clicked = Some(i);
                }
            }
        });
    clicked
}

/// One square icon button.
pub(super) fn button(
    ui: &mut egui::Ui,
    glyph: Glyph,
    side: f32,
    enabled: bool,
    on: bool,
    hint: &str,
) -> egui::Response {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(side, side), sense);
    if ui.is_rect_visible(rect) {
        let th = crate::theme::live(ui.ctx());
        let painter = ui.painter();
        if enabled && (response.hovered() || response.is_pointer_button_down_on()) {
            let fill = ui.style().interact(&response).weak_bg_fill;
            painter.rect_filled(rect, 3.0, fill);
        } else if on {
            painter.rect_filled(rect, 3.0, ui.visuals().widgets.inactive.weak_bg_fill);
        }
        let color = if enabled { th.text } else { th.dim };
        paint(painter, glyph, rect, color);
    }
    response.on_hover_text(hint)
}

fn paint(painter: &egui::Painter, glyph: Glyph, rect: egui::Rect, color: egui::Color32) {
    // Draw on a 22-unit grid centered in `rect`; `u` maps a unit to pixels.
    let u = rect.width() / 22.0;
    let c = rect.center();
    let p = |x: f32, y: f32| c + egui::vec2(x * u, y * u);
    let stroke = egui::Stroke::new((1.4 * u).max(1.0), color);
    let line = |pts: &[(f32, f32)]| {
        painter.add(egui::Shape::line(
            pts.iter().map(|&(x, y)| p(x, y)).collect(),
            stroke,
        ));
    };
    match glyph {
        Glyph::Refresh => {
            // Three quarters of a circle, with an arrowhead at its end.
            let arc: Vec<(f32, f32)> = (0..=18)
                .map(|i| {
                    let a = (40.0 + i as f32 * 15.0).to_radians();
                    (6.0 * a.cos(), -6.0 * a.sin())
                })
                .collect();
            line(&arc);
            let (x, y) = arc[arc.len() - 1];
            line(&[(x - 1.0, y - 4.5), (x, y), (x + 4.5, y - 1.0)]);
        }
        Glyph::Push => {
            line(&[(0.0, 6.0), (0.0, -6.0)]);
            line(&[(-4.5, -1.5), (0.0, -6.0), (4.5, -1.5)]);
            line(&[(-6.0, 9.0), (6.0, 9.0)]);
        }
        Glyph::Add => {
            line(&[(-6.0, 0.0), (6.0, 0.0)]);
            line(&[(0.0, -6.0), (0.0, 6.0)]);
        }
        Glyph::Directories => {
            line(&[(-6.0, -5.5), (6.0, -5.5)]);
            line(&[(-1.0, 0.0), (6.0, 0.0)]);
            line(&[(-1.0, 5.5), (6.0, 5.5)]);
            line(&[(-4.5, -5.5), (-4.5, 5.5), (-2.0, 5.5)]);
            line(&[(-4.5, 0.0), (-2.0, 0.0)]);
        }
        Glyph::Expand => {
            // Chevrons pointing away from the middle.
            line(&[(-4.5, -2.0), (0.0, -6.5), (4.5, -2.0)]);
            line(&[(-4.5, 2.0), (0.0, 6.5), (4.5, 2.0)]);
        }
        Glyph::Collapse => {
            // Chevrons pointing at the middle.
            line(&[(-4.5, -6.5), (0.0, -2.0), (4.5, -6.5)]);
            line(&[(-4.5, 6.5), (0.0, 2.0), (4.5, 6.5)]);
        }
        Glyph::Chevron => line(&[(-2.5, -5.0), (2.5, 0.0), (-2.5, 5.0)]),
        Glyph::Dots => {
            for x in [-5.0, 0.0, 5.0] {
                painter.circle_filled(p(x, 0.0), 1.4 * u, color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::visible;

    #[test]
    fn everything_fits_or_the_last_slot_becomes_the_chevron() {
        // 22px buttons, 2px gaps: 6 need 6*22 + 5*2 = 142.
        assert_eq!(visible(6, 142.0, 22.0, 2.0), 6);
        assert_eq!(visible(6, 141.0, 22.0, 2.0), 4);
        assert_eq!(visible(6, 100.0, 22.0, 2.0), 3);
        assert_eq!(visible(6, 22.0, 22.0, 2.0), 0);
        assert_eq!(visible(6, 0.0, 22.0, 2.0), 0);
        assert_eq!(visible(0, 0.0, 22.0, 2.0), 0);
    }
}
