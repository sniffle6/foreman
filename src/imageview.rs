//! `Content::Image` window: a decoded PNG shown fit-to-window with Ctrl+Scroll
//! zoom (around the pointer) and drag-to-pan. No PTY, no membership — a static
//! viewer. See docs/image-viewer.md.

use eframe::egui;
use std::path::PathBuf;

/// Zoom range, expressed as a multiple of the fit-to-window scale.
pub const ZOOM_MIN: f32 = 0.1;
pub const ZOOM_MAX: f32 = 32.0;

/// Multiplicative step per accumulated wheel notch (Ctrl+Scroll).
const ZOOM_STEP: f32 = 0.1;

pub fn clamp_zoom(z: f32) -> f32 {
    z.clamp(ZOOM_MIN, ZOOM_MAX)
}

/// Fit-to-window draw rect for an image of `img_size` pixels inside `avail`.
/// At `zoom == 1.0` the whole image is visible, aspect kept, letterboxed
/// (centered on whichever axis has slack). `zoom` scales up/down from there;
/// `pan` offsets the rect's center from `avail`'s center.
pub fn fit_rect(img_size: egui::Vec2, avail: egui::Rect, zoom: f32, pan: egui::Vec2) -> egui::Rect {
    if img_size.x <= 0.0 || img_size.y <= 0.0 || avail.width() <= 0.0 || avail.height() <= 0.0 {
        return egui::Rect::from_center_size(avail.center(), egui::Vec2::ZERO);
    }
    let base_scale = (avail.width() / img_size.x).min(avail.height() / img_size.y);
    let scale = base_scale * zoom;
    let size = img_size * scale;
    egui::Rect::from_center_size(avail.center() + pan, size)
}

/// New pan so the image point under `cursor` stays fixed on screen while
/// zooming from `old_zoom` to `new_zoom`. Falls back to the unchanged `pan`
/// when the old rect is degenerate (zero-size avail/image).
pub fn zoom_around(
    img_size: egui::Vec2,
    avail: egui::Rect,
    old_zoom: f32,
    pan: egui::Vec2,
    cursor: egui::Pos2,
    new_zoom: f32,
) -> egui::Vec2 {
    let old_rect = fit_rect(img_size, avail, old_zoom, pan);
    if old_rect.width() <= f32::EPSILON || old_rect.height() <= f32::EPSILON {
        return pan;
    }
    let fx = (cursor.x - old_rect.min.x) / old_rect.width();
    let fy = (cursor.y - old_rect.min.y) / old_rect.height();
    let new_size = fit_rect(img_size, avail, new_zoom, egui::Vec2::ZERO).size();
    let new_min = egui::pos2(cursor.x - fx * new_size.x, cursor.y - fy * new_size.y);
    let new_center = new_min + new_size / 2.0;
    new_center - avail.center()
}

/// Clamp `pan` so the image rect at `zoom` can't drift entirely off `avail`:
/// when the rect is smaller than `avail` on an axis it can't pan on that axis
/// at all (it's centered); otherwise pan is bounded so an edge never passes
/// the opposite `avail` edge.
pub fn clamp_pan(
    img_size: egui::Vec2,
    avail: egui::Rect,
    zoom: f32,
    pan: egui::Vec2,
) -> egui::Vec2 {
    let size = fit_rect(img_size, avail, zoom, egui::Vec2::ZERO).size();
    let max_x = ((size.x - avail.width()) / 2.0).max(0.0);
    let max_y = ((size.y - avail.height()) / 2.0).max(0.0);
    egui::vec2(pan.x.clamp(-max_x, max_x), pan.y.clamp(-max_y, max_y))
}

enum ImgState {
    Ok {
        w: u32,
        h: u32,
        rgba: Vec<u8>,
        texture: Option<egui::TextureHandle>,
    },
    Err(String),
}

pub struct ImageView {
    pub path: PathBuf,
    pub zoom: f32,
    pub pan: egui::Vec2,
    /// Sub-notch remainder of Ctrl+Scroll zoom, same unit/accumulate idiom as
    /// the terminal's font zoom (`Session::zoom_accum`, `input::wheel_steps`).
    zoom_accum: f32,
    img: ImgState,
}

impl ImageView {
    /// Pure error-state constructor: never panics, never touches disk. Used
    /// directly by tests and by `load`'s failure paths.
    pub fn error(path: PathBuf, msg: impl Into<String>) -> Self {
        Self {
            path,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            zoom_accum: 0.0,
            img: ImgState::Err(msg.into()),
        }
    }

    /// Decode already-read bytes. Pure (no disk I/O) — the seam TDD'd here;
    /// `load` is the thin disk-reading wrapper around it.
    pub fn from_bytes(path: PathBuf, data: &[u8]) -> Self {
        match crate::graphics::decode_png(data) {
            Ok((w, h, rgba)) => Self {
                path,
                zoom: 1.0,
                pan: egui::Vec2::ZERO,
                zoom_accum: 0.0,
                img: ImgState::Ok {
                    w,
                    h,
                    rgba,
                    texture: None,
                },
            },
            Err(e) => Self::error(path, format!("decode error: {e}")),
        }
    }

    /// Read `path` from disk and decode it. Missing file / bad PNG both land
    /// in the error state — never a panic, never a dead window.
    pub fn load(path: PathBuf) -> Self {
        match std::fs::read(&path) {
            Ok(data) => Self::from_bytes(path, &data),
            Err(e) => Self::error(path, format!("cannot read file: {e}")),
        }
    }

    #[cfg(test)]
    fn is_err(&self) -> bool {
        matches!(self.img, ImgState::Err(_))
    }

    #[cfg(test)]
    fn is_ok(&self) -> bool {
        matches!(self.img, ImgState::Ok { .. })
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        active: bool,
        resp: &egui::Response,
    ) {
        let th = crate::theme::live(ui.ctx());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, egui::CornerRadius::ZERO, th.bg);

        match &mut self.img {
            ImgState::Err(msg) => {
                let text = format!("{}\n{msg}", self.path.display());
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    text,
                    egui::FontId::proportional(13.0),
                    th.dim,
                );
            }
            ImgState::Ok {
                w,
                h,
                rgba,
                texture,
            } => {
                let img_size = egui::vec2(*w as f32, *h as f32);

                if resp.hovered() {
                    let (dy, ctrl) = ui.input(|i| {
                        (
                            i.smooth_scroll_delta.y,
                            i.modifiers.ctrl || i.modifiers.command,
                        )
                    });
                    if ctrl && dy != 0.0 {
                        let (steps, rem) = crate::input::wheel_steps(
                            self.zoom_accum,
                            dy,
                            crate::input::WHEEL_NOTCH_PX,
                        );
                        self.zoom_accum = rem;
                        if steps != 0.0 {
                            if let Some(cursor) = resp.hover_pos() {
                                let new_zoom = clamp_zoom(self.zoom * (1.0 + steps * ZOOM_STEP));
                                self.pan = zoom_around(
                                    img_size, rect, self.zoom, self.pan, cursor, new_zoom,
                                );
                                self.zoom = new_zoom;
                                self.pan = clamp_pan(img_size, rect, self.zoom, self.pan);
                            }
                        }
                    }
                }
                if active
                    && ui.input(|i| {
                        (i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(egui::Key::Num0)
                    })
                {
                    self.zoom = 1.0;
                    self.pan = egui::Vec2::ZERO;
                }
                if resp.dragged() {
                    self.pan += resp.drag_delta();
                    self.pan = clamp_pan(img_size, rect, self.zoom, self.pan);
                }

                let draw_rect = fit_rect(img_size, rect, self.zoom, self.pan);
                let tex = texture.get_or_insert_with(|| {
                    let color_img =
                        egui::ColorImage::from_rgba_unmultiplied([*w as usize, *h as usize], rgba);
                    ui.ctx().load_texture(
                        format!("imageview_{}", self.path.display()),
                        color_img,
                        egui::TextureOptions::LINEAR,
                    )
                });
                let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                painter.image(tex.id(), draw_rect, uv, egui::Color32::WHITE);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(w: f32, h: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h))
    }

    // ── fit_rect ────────────────────────────────────────────────────────

    #[test]
    fn fit_rect_letterboxes_wide_image_in_tall_window() {
        // 200x100 image (2:1) in a 100x100 window: fit width, letterbox top/bottom.
        let r = fit_rect(
            egui::vec2(200.0, 100.0),
            rect(100.0, 100.0),
            1.0,
            egui::Vec2::ZERO,
        );
        assert!((r.width() - 100.0).abs() < 0.01, "{r:?}");
        assert!((r.height() - 50.0).abs() < 0.01, "{r:?}");
        // centered
        assert!((r.center().x - 50.0).abs() < 0.01);
        assert!((r.center().y - 50.0).abs() < 0.01);
    }

    #[test]
    fn fit_rect_letterboxes_tall_image_in_wide_window() {
        let r = fit_rect(
            egui::vec2(100.0, 200.0),
            rect(200.0, 100.0),
            1.0,
            egui::Vec2::ZERO,
        );
        assert!((r.width() - 50.0).abs() < 0.01, "{r:?}");
        assert!((r.height() - 100.0).abs() < 0.01, "{r:?}");
    }

    #[test]
    fn fit_rect_scales_with_zoom() {
        let base = fit_rect(
            egui::vec2(100.0, 100.0),
            rect(200.0, 200.0),
            1.0,
            egui::Vec2::ZERO,
        );
        let zoomed = fit_rect(
            egui::vec2(100.0, 100.0),
            rect(200.0, 200.0),
            2.0,
            egui::Vec2::ZERO,
        );
        assert!((zoomed.width() - base.width() * 2.0).abs() < 0.01);
    }

    #[test]
    fn fit_rect_applies_pan_as_center_offset() {
        let r = fit_rect(
            egui::vec2(100.0, 100.0),
            rect(200.0, 200.0),
            1.0,
            egui::vec2(30.0, -10.0),
        );
        assert!((r.center().x - 130.0).abs() < 0.01);
        assert!((r.center().y - 90.0).abs() < 0.01);
    }

    #[test]
    fn fit_rect_degenerate_avail_is_a_point_not_a_panic() {
        let r = fit_rect(
            egui::vec2(100.0, 100.0),
            rect(0.0, 0.0),
            1.0,
            egui::Vec2::ZERO,
        );
        assert_eq!(r.width(), 0.0);
        assert_eq!(r.height(), 0.0);
    }

    // ── zoom_around ─────────────────────────────────────────────────────

    #[test]
    fn zoom_around_keeps_cursor_point_fixed() {
        let img = egui::vec2(100.0, 100.0);
        let avail = rect(200.0, 200.0);
        let cursor = egui::pos2(120.0, 80.0);
        let old_zoom = 1.0;
        let pan = egui::Vec2::ZERO;
        let new_zoom = 2.0;
        let new_pan = zoom_around(img, avail, old_zoom, pan, cursor, new_zoom);

        let old_rect = fit_rect(img, avail, old_zoom, pan);
        let fx = (cursor.x - old_rect.min.x) / old_rect.width();
        let fy = (cursor.y - old_rect.min.y) / old_rect.height();

        let new_rect = fit_rect(img, avail, new_zoom, new_pan);
        let nx = new_rect.min.x + fx * new_rect.width();
        let ny = new_rect.min.y + fy * new_rect.height();
        assert!(
            (nx - cursor.x).abs() < 0.05,
            "nx={nx} cursor.x={}",
            cursor.x
        );
        assert!(
            (ny - cursor.y).abs() < 0.05,
            "ny={ny} cursor.y={}",
            cursor.y
        );
    }

    #[test]
    fn zoom_around_degenerate_old_rect_returns_unchanged_pan() {
        let pan = egui::vec2(5.0, 7.0);
        let new_pan = zoom_around(
            egui::vec2(100.0, 100.0),
            rect(0.0, 0.0),
            1.0,
            pan,
            egui::pos2(0.0, 0.0),
            2.0,
        );
        assert_eq!(new_pan, pan);
    }

    // ── clamp_pan ───────────────────────────────────────────────────────

    #[test]
    fn clamp_pan_locks_to_zero_when_image_fits_without_scrolling() {
        // At zoom 1.0, fit_rect fills avail exactly on the shorter axis and the
        // image never exceeds avail size, so any pan collapses to zero.
        let p = clamp_pan(
            egui::vec2(100.0, 100.0),
            rect(200.0, 200.0),
            1.0,
            egui::vec2(999.0, -999.0),
        );
        assert_eq!(p, egui::Vec2::ZERO);
    }

    #[test]
    fn clamp_pan_bounds_to_half_the_overhang_when_zoomed_in() {
        // zoom 2.0 on a 100x100 image fit into 200x200 avail -> 400x400 draw
        // size; overhang per axis = (400-200)/2 = 100.
        let p = clamp_pan(
            egui::vec2(100.0, 100.0),
            rect(200.0, 200.0),
            2.0,
            egui::vec2(500.0, -500.0),
        );
        assert!((p.x - 100.0).abs() < 0.01, "{p:?}");
        assert!((p.y - -100.0).abs() < 0.01, "{p:?}");
    }

    // ── clamp_zoom ──────────────────────────────────────────────────────

    #[test]
    fn clamp_zoom_bounds_to_range() {
        assert_eq!(clamp_zoom(0.0), ZOOM_MIN);
        assert_eq!(clamp_zoom(1000.0), ZOOM_MAX);
        assert_eq!(clamp_zoom(1.0), 1.0);
    }

    // ── error state / decode ────────────────────────────────────────────

    #[test]
    fn error_constructor_never_touches_disk_and_carries_the_message() {
        let v = ImageView::error(PathBuf::from(r"C:\nope\missing.png"), "cannot read file: x");
        assert!(v.is_err());
        assert!(!v.is_ok());
        assert_eq!(v.zoom, 1.0);
        assert_eq!(v.pan, egui::Vec2::ZERO);
    }

    #[test]
    fn from_bytes_decodes_a_valid_png() {
        // Real PNG bytes generated with the `png` crate — a 2x2 opaque red
        // square — so decode exercises the actual codec, not a fixture.
        let mut png_bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_bytes, 2, 2);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[255, 0, 0, 255].repeat(4)).unwrap();
        }
        let v = ImageView::from_bytes(PathBuf::from("t.png"), &png_bytes);
        assert!(v.is_ok(), "expected a decoded image");
    }

    #[test]
    fn from_bytes_bad_data_lands_in_error_state_not_a_panic() {
        let v = ImageView::from_bytes(PathBuf::from("bad.png"), b"not a png");
        assert!(v.is_err());
    }

    #[test]
    fn load_missing_file_lands_in_error_state() {
        let v = ImageView::load(PathBuf::from(r"H:\definitely\does\not\exist_9f8c.png"));
        assert!(v.is_err());
    }
}
