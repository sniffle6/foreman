//! A row of icon buttons that folds what doesn't fit behind a ">" menu, like
//! the JetBrains tool-window toolbars. The icons are Google Material Symbols
//! (`assets/icons/material/`), rasterized and tinted by `crate::icons`, so
//! they scale with view zoom and need no font glyphs. A tool with `choices`
//! is not a click action: hovering it pops up its options at once, like the
//! eye (View Options) button in JetBrains.

use crate::icons::{self, material};
use eframe::egui;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Glyph {
    Refresh,
    Push,
    Add,
    /// The eye: how the files are shown.
    View,
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
            Glyph::View => material::VISIBILITY,
            Glyph::Expand => material::UNFOLD_MORE,
            Glyph::Collapse => material::UNFOLD_LESS,
            Glyph::Chevron => material::CHEVRON_RIGHT,
            Glyph::Dots => material::MORE_HORIZ,
        }
    }
}

/// One option of a tool with choices; `on`: the current one.
pub(super) struct Choice {
    pub label: String,
    pub on: bool,
}

/// One toolbar button. `label` is its name in the overflow menu, `hint` its
/// tooltip. `on`: a toggle that is currently on. `choices`, when not empty,
/// make the button a chooser: hovering it shows them, and the overflow menu
/// lists them in its place.
pub(super) struct Tool {
    pub glyph: Glyph,
    pub label: String,
    pub hint: String,
    pub enabled: bool,
    pub on: bool,
    pub choices: Vec<Choice>,
}

/// What was clicked: a tool, or one of its choices.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Click {
    pub tool: usize,
    pub choice: Option<usize>,
}

/// The icon's share of a `side`-square button: a 16px glyph in a 22px button.
const ICON_SHARE: f32 = 16.0 / 22.0;

/// How many of `n` buttons fit in `avail`; when they don't all, the last slot
/// goes to the ">" button.
fn visible(n: usize, avail: f32, side: f32, gap: f32) -> usize {
    let width = |k: usize| k as f32 * side + k.saturating_sub(1) as f32 * gap;
    if width(n) <= avail {
        return n;
    }
    (0..n).rev().find(|&k| width(k + 1) <= avail).unwrap_or(0)
}

/// Draw `tools`; returns what was clicked, in the row, in a chooser's popup,
/// or in the overflow menu.
pub(super) fn show(ui: &mut egui::Ui, tools: &[Tool], scale: f32) -> Option<Click> {
    let side = 22.0 * scale;
    let gap = 2.0 * scale;
    let shown = visible(tools.len(), ui.available_width(), side, gap);
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (i, tool) in tools.iter().take(shown).enumerate() {
            let response = icon(ui, tool.glyph, side, tool.enabled, tool.on);
            if tool.choices.is_empty() {
                if response.on_hover_text(&tool.hint).clicked() {
                    clicked = Some(Click {
                        tool: i,
                        choice: None,
                    });
                }
            } else if let Some(c) =
                hover_popup(&response, |ui| choices(ui, &tool.choices)).flatten()
            {
                clicked = Some(Click {
                    tool: i,
                    choice: Some(c),
                });
            }
        }
        if shown < tools.len() {
            let more = button(ui, Glyph::Chevron, side, true, false, "More actions");
            if let Some(mut click) = menu(&more, &tools[shown..]) {
                click.tool += shown;
                clicked = Some(click);
            }
        }
    });
    clicked
}

/// Between a chooser and its popup; the popup also counts this margin as
/// itself, so the pointer can cross it without the popup closing.
const POPUP_GAP: f32 = 4.0;

/// A popup under `anchor` that opens the moment it is hovered (not after
/// egui's tooltip delay) and stays while the pointer is in it, so its
/// buttons can be clicked. Returns what `content` returned, if shown.
fn hover_popup<R>(anchor: &egui::Response, content: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
    let ctx = &anchor.ctx;
    let id = anchor.id.with("hover-popup");
    // Only a popup shown last frame can hold itself open; its area rect
    // outlives it in memory, so that alone would reopen it later.
    let shown = ctx.read_response(id).is_some();
    let inside = shown
        && egui::AreaState::load(ctx, id)
            .zip(ctx.pointer_hover_pos())
            .is_some_and(|(area, pos)| area.rect().expand(POPUP_GAP).contains(pos));
    egui::Popup::from_response(anchor)
        .id(id)
        .kind(egui::PopupKind::Tooltip)
        .gap(POPUP_GAP)
        .open(anchor.hovered() || inside)
        .show(content)
        .map(|r| r.inner)
}

/// `choices` as selectable rows; returns the clicked one.
fn choices(ui: &mut egui::Ui, choices: &[Choice]) -> Option<usize> {
    let mut clicked = None;
    for (i, choice) in choices.iter().enumerate() {
        if ui
            .add(egui::Button::new(&choice.label).selected(choice.on))
            .clicked()
        {
            clicked = Some(i);
        }
    }
    clicked
}

/// The menu behind a button: `tools` as labeled rows, a chooser as its
/// choices. Returns the clicked one.
fn menu(anchor: &egui::Response, tools: &[Tool]) -> Option<Click> {
    let mut clicked = None;
    egui::Popup::menu(anchor)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            for (i, tool) in tools.iter().enumerate() {
                if !tool.choices.is_empty() {
                    if let Some(c) = choices(ui, &tool.choices) {
                        clicked = Some(Click {
                            tool: i,
                            choice: Some(c),
                        });
                    }
                    continue;
                }
                let item = ui.add_enabled(
                    tool.enabled,
                    egui::Button::new(&tool.label).selected(tool.on),
                );
                let item = item
                    .on_hover_text(&tool.hint)
                    .on_disabled_hover_text(&tool.hint);
                if item.clicked() {
                    clicked = Some(Click {
                        tool: i,
                        choice: None,
                    });
                }
            }
        });
    clicked
}

/// The menu behind a button, for plain `tools` (no choices): the index of
/// the clicked one.
pub(super) fn overflow(anchor: &egui::Response, tools: &[Tool]) -> Option<usize> {
    menu(anchor, tools).map(|c| c.tool)
}

/// One square icon button with its tooltip.
pub(super) fn button(
    ui: &mut egui::Ui,
    glyph: Glyph,
    side: f32,
    enabled: bool,
    on: bool,
    hint: &str,
) -> egui::Response {
    icon(ui, glyph, side, enabled, on).on_hover_text(hint)
}

/// One square icon button, no tooltip. The icon is the tinted Material
/// Symbol texture, rasterized at the device pixel size so it stays crisp at
/// any zoom or DPI.
fn icon(ui: &mut egui::Ui, glyph: Glyph, side: f32, enabled: bool, on: bool) -> egui::Response {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(side, side), sense);
    if ui.is_rect_visible(rect) {
        let th = crate::theme::live(ui.ctx());
        let size = side * ICON_SHARE;
        let px = (size * ui.ctx().pixels_per_point()).ceil().max(1.0) as u32;
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
        painter.image(
            texture.id(),
            egui::Rect::from_center_size(rect.center(), egui::vec2(size, size)),
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            color,
        );
    }
    response
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
