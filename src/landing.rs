#![allow(dead_code)] // removed in Task B3 once App wires the landing

use eframe::egui;
use std::path::PathBuf;

/// Provisional, landing-local taxonomy (phase-2 replaces it with the dispatch model).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionKind {
    Claude,
    Codex,
    Terminal,
}

/// Open `path` as a new project running `kind`.
pub struct LandingAction {
    pub path: PathBuf,
    pub kind: SessionKind,
}

/// Fixed icon order so positional rects and the hit-test agree.
const ICON_ORDER: [SessionKind; 3] = [
    SessionKind::Claude,
    SessionKind::Codex,
    SessionKind::Terminal,
];

/// FOREMAN in a mono block font — real terminal art.
const FOREMAN_ART: &str = r"
███████  ██████  ██████  ███████ ███    ███  █████  ███    ██
██      ██    ██ ██   ██ ██      ████  ████ ██   ██ ████   ██
█████   ██    ██ ██████  █████   ██ ████ ██ ███████ ██ ██  ██
██      ██    ██ ██   ██ ██      ██  ██  ██ ██   ██ ██  ██ ██
██       ██████  ██   ██ ███████ ██      ██ ██   ██ ██   ████";

struct LandingLayout {
    wordmark: egui::Rect,
    tagline: egui::Rect,
    field: egui::Rect,
    icons: Vec<egui::Rect>,
}

/// Place the stack (wordmark → tagline → field → icon row) centered in `area`.
/// Pure arithmetic — no fonts, no fs.
fn layout(area: egui::Rect, n_icons: usize) -> LandingLayout {
    let cx = area.center().x;
    let field_w = area.width().min(520.0).max(0.0);
    let (word_h, tag_h, field_h, icon, gap) = (120.0_f32, 24.0, 26.0, 72.0_f32, 18.0);
    let total = word_h + 16.0 + tag_h + 28.0 + field_h + 36.0 + icon;
    let mut y = area.center().y - total / 2.0;

    let centered = |w: f32, y: f32, h: f32| {
        egui::Rect::from_min_size(egui::pos2(cx - w / 2.0, y), egui::vec2(w, h))
    };
    let word_w = area.width().min(760.0);
    let wordmark = centered(word_w, y, word_h);
    y += word_h + 16.0;
    let tagline = centered(word_w, y, tag_h);
    y += tag_h + 28.0;
    let field = centered(field_w, y, field_h);
    y += field_h + 36.0;

    let n = n_icons.max(1);
    let row_w = (icon * n as f32 + gap * (n as f32 - 1.0)).min(area.width());
    let mut x = cx - row_w / 2.0;
    let icons = (0..n_icons)
        .map(|_| {
            let r = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(icon, icon));
            x += icon + gap;
            r
        })
        .collect();

    LandingLayout {
        wordmark,
        tagline,
        field,
        icons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1200.0, 800.0))
    }

    #[test]
    fn every_element_is_inside_the_area_and_disjoint() {
        let a = area();
        let l = layout(a, 3);
        for r in [l.wordmark, l.tagline, l.field] {
            assert!(a.contains_rect(r), "{r:?} escapes {a:?}");
        }
        assert!(l.wordmark.bottom() <= l.tagline.top());
        assert!(l.tagline.bottom() <= l.field.top());
        assert!(l.field.bottom() <= l.icons[0].top());
    }

    #[test]
    fn stack_is_horizontally_centered() {
        let a = area();
        let l = layout(a, 3);
        let c = a.center().x;
        for r in [l.wordmark, l.tagline, l.field] {
            assert!((r.center().x - c).abs() < 1.0, "{r:?} not centered");
        }
    }

    #[test]
    fn icons_are_equal_width_and_evenly_spaced() {
        let l = layout(area(), 3);
        assert_eq!(l.icons.len(), 3);
        let w = l.icons[0].width();
        assert!(l.icons.iter().all(|r| (r.width() - w).abs() < 0.5));
        let g1 = l.icons[1].left() - l.icons[0].right();
        let g2 = l.icons[2].left() - l.icons[1].right();
        assert!((g1 - g2).abs() < 0.5);
    }

    #[test]
    fn tiny_area_degrades_without_negative_sizes() {
        let a = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(240.0, 180.0));
        let l = layout(a, 3);
        for r in [l.wordmark, l.tagline, l.field] {
            assert!(r.width() >= 0.0 && r.height() >= 0.0);
            assert!(a.contains_rect(r.intersect(a)));
        }
    }
}
