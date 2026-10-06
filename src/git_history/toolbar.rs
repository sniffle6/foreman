//! A row of icon buttons that folds what doesn't fit behind a ">" menu, like
//! the JetBrains tool-window toolbars. The icons are Google Material Symbols
//! (`assets/icons/material/`), rasterized and tinted by `crate::icons`, so
//! they scale with view zoom and need no font glyphs. A `labeled` tool shows
//! its name beside its icon, but only while every tool fits that way;
//! otherwise the row is icons only and folds as usual.

use crate::icons::{self, material};
use eframe::egui;
use std::sync::Arc;

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

impl Glyph {
    /// The Material Symbol: its cache name and SVG.
    fn svg(self) -> (&'static str, &'static str) {
        match self {
            Glyph::Refresh => material::REFRESH,
            Glyph::Push => material::UPLOAD,
            Glyph::Add => material::ADD,
            Glyph::Directories => material::ACCOUNT_TREE,
            Glyph::Expand => material::UNFOLD_MORE,
            Glyph::Collapse => material::UNFOLD_LESS,
            Glyph::Chevron => material::CHEVRON_RIGHT,
            Glyph::Dots => material::MORE_HORIZ,
        }
    }
}

/// One toolbar button. `label` is its name in the overflow menu, `hint` its
/// tooltip. `on`: a toggle that is currently on. `labeled`: also show the
/// name beside the icon when the whole row fits that way.
pub(super) struct Tool {
    pub glyph: Glyph,
    pub label: String,
    pub hint: String,
    pub enabled: bool,
    pub on: bool,
    pub labeled: bool,
}

/// The icon's share of a `side`-square button: a 16px glyph in a 22px button.
const ICON_SHARE: f32 = 16.0 / 22.0;
/// Padding after a labeled button's text, as a share of `side`.
const LABEL_PAD_SHARE: f32 = 6.0 / 22.0;

/// Width of a button whose label (`text` wide) sits beside its icon.
fn labeled_width(side: f32, text: f32) -> f32 {
    side + text + side * LABEL_PAD_SHARE
}

/// How many buttons fit in `avail`, given each one's width: all of them, or
/// as many as leave room for the ">" button (`side` wide) after them.
fn visible(widths: &[f32], avail: f32, side: f32, gap: f32) -> usize {
    let n = widths.len();
    let row = |k: usize| widths[..k].iter().sum::<f32>() + k.saturating_sub(1) as f32 * gap;
    if row(n) <= avail {
        return n;
    }
    // k buttons, each followed by a gap, then the chevron.
    (0..n)
        .rev()
        .find(|&k| widths[..k].iter().sum::<f32>() + k as f32 * gap + side <= avail)
        .unwrap_or(0)
}

/// How to draw the row: `(shown, labeled)`. `labeled` holds the buttons'
/// widths with their labels beside the icons (`side` for an unlabeled one);
/// when all of them fit that way, that's the row. Otherwise every button is
/// `side` wide and the ones that don't fit fold behind the chevron.
fn plan(labeled: &[f32], avail: f32, side: f32, gap: f32) -> (usize, bool) {
    let n = labeled.len();
    if visible(labeled, avail, side, gap) == n {
        return (n, true);
    }
    (visible(&vec![side; n], avail, side, gap), false)
}

/// Draw `tools`; returns the index of the one clicked, from the row or from
/// the overflow menu.
pub(super) fn show(ui: &mut egui::Ui, tools: &[Tool], scale: f32) -> Option<usize> {
    let side = 22.0 * scale;
    let gap = 2.0 * scale;
    let font = egui::FontId::proportional(13.0 * scale);
    let labels: Vec<Option<Arc<egui::Galley>>> = tools
        .iter()
        .map(|t| {
            t.labeled.then(|| {
                ui.painter().layout_no_wrap(
                    t.label.clone(),
                    font.clone(),
                    egui::Color32::PLACEHOLDER,
                )
            })
        })
        .collect();
    let widths: Vec<f32> = labels
        .iter()
        .map(|g| g.as_ref().map_or(side, |g| labeled_width(side, g.size().x)))
        .collect();
    let (shown, labeled) = plan(&widths, ui.available_width(), side, gap);
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (i, tool) in tools.iter().take(shown).enumerate() {
            let label = if labeled { labels[i].clone() } else { None };
            let response = widget(
                ui,
                tool.glyph,
                side,
                label,
                tool.enabled,
                tool.on,
                &tool.hint,
            );
            if response.clicked() {
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
    widget(ui, glyph, side, None, enabled, on, hint)
}

/// An icon button `side` high: square, or widened by `label` drawn after the
/// icon. The icon is the tinted Material Symbol texture, rasterized at the
/// device pixel size so it stays crisp at any zoom or DPI.
fn widget(
    ui: &mut egui::Ui,
    glyph: Glyph,
    side: f32,
    label: Option<Arc<egui::Galley>>,
    enabled: bool,
    on: bool,
    hint: &str,
) -> egui::Response {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let width = label
        .as_ref()
        .map_or(side, |g| labeled_width(side, g.size().x));
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, side), sense);
    if ui.is_rect_visible(rect) {
        let th = crate::theme::live(ui.ctx());
        let icon = side * ICON_SHARE;
        let px = (icon * ui.ctx().pixels_per_point()).ceil().max(1.0) as u32;
        let (name, svg) = glyph.svg();
        let texture = icons::texture_svg(ui.ctx(), name, svg, px);
        let painter = ui.painter();
        if enabled && (response.hovered() || response.is_pointer_button_down_on()) {
            let fill = ui.style().interact(&response).weak_bg_fill;
            painter.rect_filled(rect, 3.0, fill);
        } else if on {
            painter.rect_filled(rect, 3.0, ui.visuals().widgets.inactive.weak_bg_fill);
        }
        let color = if enabled { th.text } else { th.dim };
        // The icon sits centered in the leading `side` square; the label, if
        // any, follows it.
        let square = egui::Rect::from_min_size(rect.min, egui::vec2(side, side));
        painter.image(
            texture.id(),
            egui::Rect::from_center_size(square.center(), egui::vec2(icon, icon)),
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            color,
        );
        if let Some(galley) = label {
            let pos = egui::pos2(square.right(), rect.center().y - galley.size().y / 2.0);
            painter.galley(pos, galley, color);
        }
    }
    response.on_hover_text(hint)
}

#[cfg(test)]
mod tests {
    use super::{labeled_width, plan, visible};

    #[test]
    fn everything_fits_or_the_last_slot_becomes_the_chevron() {
        // 22px buttons, 2px gaps: 6 need 6*22 + 5*2 = 142.
        let six = [22.0; 6];
        assert_eq!(visible(&six, 142.0, 22.0, 2.0), 6);
        assert_eq!(visible(&six, 141.0, 22.0, 2.0), 4);
        assert_eq!(visible(&six, 100.0, 22.0, 2.0), 3);
        assert_eq!(visible(&six, 22.0, 22.0, 2.0), 0);
        assert_eq!(visible(&six, 0.0, 22.0, 2.0), 0);
        assert_eq!(visible(&[], 0.0, 22.0, 2.0), 0);
        // A wide button counts its own width: 22 + 60 + 22 with two gaps.
        let mixed = [22.0, 60.0, 22.0];
        assert_eq!(visible(&mixed, 108.0, 22.0, 2.0), 3);
        // One short: the wide one and the last fold, since 22+2+60+2+22 > 107.
        assert_eq!(visible(&mixed, 107.0, 22.0, 2.0), 1);
    }

    #[test]
    fn labels_show_only_when_the_whole_row_fits_with_them() {
        // Three buttons, the middle one labeled ("Push…", 40px of text):
        // 22 + (22 + 40 + 6) + 22, two gaps = 116.
        let labeled = [22.0, labeled_width(22.0, 40.0), 22.0];
        assert_eq!(plan(&labeled, 116.0, 22.0, 2.0), (3, true));
        // A pixel short of that, and the row is icons only (70 wide): all fit.
        assert_eq!(plan(&labeled, 115.0, 22.0, 2.0), (3, false));
        assert_eq!(plan(&labeled, 70.0, 22.0, 2.0), (3, false));
        // Narrower still, and icons fold behind the chevron as always.
        assert_eq!(plan(&labeled, 69.0, 22.0, 2.0), (1, false));
        assert_eq!(plan(&[], 0.0, 22.0, 2.0), (0, true));
    }
}
