//! Shared font zoom for native view widgets. egui does not transform custom
//! painter coordinates or explicit widget sizes; those use [`ViewScale::px`]
//! or [`ViewScale::factor`] at the call site.
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

    /// Give a view's child UI the context's current style at this font scale.
    /// This is idempotent: a child can inherit an already scaled style without
    /// doubling it. Apply local style overrides after this call.
    pub fn apply(self, ui: &mut egui::Ui) {
        ui.set_style(self.scaled_style(ui.ctx()));
    }

    /// Detached egui popups start from the context style, not their trigger's
    /// child UI. Give them the same scale explicitly.
    pub fn popup_style(self) -> egui::style::StyleModifier {
        egui::style::StyleModifier::new(move |style| Self::scale_style(style, self.0))
    }

    pub fn px(self, logical_px: f32) -> f32 {
        logical_px * self.0
    }

    fn scaled_style(self, ctx: &egui::Context) -> egui::Style {
        let mut style = (*ctx.global_style()).clone();
        Self::scale_style(&mut style, self.0);
        style
    }

    fn scale_style(style: &mut egui::Style, factor: f32) {
        let margin = |m: egui::Margin| egui::Margin {
            left: (m.left as f32 * factor).round() as i8,
            right: (m.right as f32 * factor).round() as i8,
            top: (m.top as f32 * factor).round() as i8,
            bottom: (m.bottom as f32 * factor).round() as i8,
        };
        let spacing = &mut style.spacing;
        spacing.item_spacing *= factor;
        spacing.window_margin = margin(spacing.window_margin);
        spacing.button_padding *= factor;
        spacing.menu_margin = margin(spacing.menu_margin);
        spacing.indent *= factor;
        spacing.interact_size *= factor;
        spacing.slider_width *= factor;
        spacing.slider_rail_height *= factor;
        spacing.combo_width *= factor;
        spacing.text_edit_width *= factor;
        spacing.icon_width *= factor;
        spacing.icon_width_inner *= factor;
        spacing.icon_spacing *= factor;
        spacing.tooltip_width *= factor;
        spacing.menu_width *= factor;
        spacing.menu_spacing *= factor;
        spacing.combo_height *= factor;
        spacing.scroll.content_margin = margin(spacing.scroll.content_margin);
        spacing.scroll.bar_width *= factor;
        spacing.scroll.handle_min_length *= factor;
        spacing.scroll.bar_inner_margin *= factor;
        spacing.scroll.bar_outer_margin *= factor;
        spacing.scroll.floating_width *= factor;
        spacing.scroll.floating_allocated_width *= factor;
        for font in style.text_styles.values_mut() {
            font.size *= factor;
        }
        if let Some(font) = &mut style.override_font_id {
            font.size *= factor;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_widgets_and_detached_popup_follow_font_zoom_without_compounding() {
        let ctx = egui::Context::default();
        let base = (*ctx.global_style()).clone();
        for factor in [0.5, 1.0, 2.0] {
            crate::terminal::set_font_size(&ctx, crate::config::DEFAULT_FONT_SIZE * factor);
            let zoom = ViewScale::from_ctx(&ctx);
            assert_eq!(zoom.px(28.0), 28.0 * factor);
            let _ = ctx.run_ui(egui::RawInput::default(), |root| {
                let mut child = root.new_child(egui::UiBuilder::new().id_salt("scaled"));
                zoom.apply(&mut child);
                let first = child.style().as_ref().clone();
                zoom.apply(&mut child);
                assert_eq!(&first, child.style().as_ref(), "apply must be idempotent");
                assert_eq!(
                    child.spacing().interact_size,
                    base.spacing.interact_size * factor
                );
                assert_eq!(
                    child.spacing().icon_spacing,
                    base.spacing.icon_spacing * factor
                );
                assert_eq!(
                    child.spacing().combo_width,
                    base.spacing.combo_width * factor
                );
                for (kind, font) in &base.text_styles {
                    assert_eq!(child.style().text_styles[kind].size, font.size * factor);
                }
                let nested = child.new_child(egui::UiBuilder::new().id_salt("nested"));
                assert_eq!(nested.style(), child.style(), "descendants inherit zoom");

                let mut popup = base.clone();
                zoom.popup_style().apply(&mut popup);
                assert_eq!(&popup, child.style().as_ref(), "detached popup matches");
            });
        }
    }
}
