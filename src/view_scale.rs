//! Shared font zoom for native views with egui widgets and painted geometry.
use eframe::egui;

#[derive(Clone, Copy)]
pub struct ViewScale(f32);

impl ViewScale {
    pub fn from_ctx(ctx: &egui::Context) -> Self {
        Self(crate::terminal::font_size(ctx) / crate::config::DEFAULT_FONT_SIZE)
    }

    pub fn factor(self) -> f32 {
        self.0
    }

    /// Apply once to a newly created child UI. Widgets added to that UI and
    /// its descendants then inherit scaled fonts and spacing by default.
    /// Custom painter coordinates still need `px` (or the scale factor).
    pub fn apply(self, ui: &mut egui::Ui) {
        let style = ui.style_mut();
        Self::scale_style(style, self.0);
    }

    pub fn px(self, logical_px: f32) -> f32 {
        logical_px * self.0
    }

    fn scale_style(style: &mut egui::Style, factor: f32) {
        let spacing = &mut style.spacing;
        spacing.item_spacing *= factor;
        spacing.button_padding *= factor;
        spacing.interact_size *= factor;
        spacing.icon_width *= factor;
        spacing.icon_width_inner *= factor;
        for font in style.text_styles.values_mut() {
            font.size *= factor;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widget_style_and_custom_pixels_follow_font_zoom() {
        let ctx = egui::Context::default();
        let base = egui::Style::default();
        for factor in [0.5, 1.0, 2.0] {
            crate::terminal::set_font_size(&ctx, crate::config::DEFAULT_FONT_SIZE * factor);
            let zoom = ViewScale::from_ctx(&ctx);
            assert_eq!(zoom.px(28.0), 28.0 * factor);
            let mut style = base.clone();
            ViewScale::scale_style(&mut style, zoom.factor());
            assert_eq!(
                style.spacing.interact_size,
                base.spacing.interact_size * factor
            );
            assert_eq!(
                style.spacing.item_spacing,
                base.spacing.item_spacing * factor
            );
            assert_eq!(
                style.spacing.button_padding,
                base.spacing.button_padding * factor
            );
            assert_eq!(style.spacing.icon_width, base.spacing.icon_width * factor);
            for (kind, font) in &base.text_styles {
                assert_eq!(style.text_styles[kind].size, font.size * factor);
            }
        }
    }
}
