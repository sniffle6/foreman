//! Cell metrics — pixel↔cell geometry for one rendered terminal frame.
//!
//! One pane's origin, monospace cell size, and grid dimensions, built once per
//! frame in `Session::show`. Every pointer→cell and cell→rect conversion goes
//! through here so the clamping arithmetic lives (and is tested) in one place
//! instead of being hand-rolled at each paint/input site.

use alacritty_terminal::vte::ansi::CursorShape;
use eframe::egui;

#[derive(Clone, Copy, Debug)]
pub(crate) struct CellMetrics {
    rect: egui::Rect,
    cw: f32,
    rh: f32,
    cols: usize,
    rows: usize,
}

impl CellMetrics {
    pub fn new(rect: egui::Rect, cw: f32, rh: f32, cols: usize, rows: usize) -> Self {
        CellMetrics {
            rect,
            cw,
            rh,
            cols,
            rows,
        }
    }

    /// The pane rect this metrics was built from. `rect().min` is the cell
    /// origin; the right/bottom edges keep up to a cell of slack past the last
    /// cell, which the scrollback thumb needs — it hugs the true pane edge, not
    /// the grid edge (`origin.x + cols*cw` falls short of `rect.max.x`).
    pub fn rect(&self) -> egui::Rect {
        self.rect
    }

    /// Cached column/row counts — the dims the pane was laid out at this frame.
    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Pointer → 0-based `(row, col)`, clamped inside the grid. The selection
    /// path: a drag that leaves the pane still resolves to the nearest cell.
    pub fn cell_at(&self, pos: egui::Pos2) -> (usize, usize) {
        let col = (((pos.x - self.rect.min.x) / self.cw).floor() as i64)
            .clamp(0, self.cols as i64 - 1) as usize;
        let row = (((pos.y - self.rect.min.y) / self.rh).floor() as i64)
            .clamp(0, self.rows as i64 - 1) as usize;
        (row, col)
    }

    /// Pointer → 1-based `(col, row)` for mouse reporting, clamped to
    /// `1..=cols`/`1..=rows`. Note the order: mouse protocols (SGR/X10) speak
    /// column-first, so this returns the opposite order to [`Self::cell_at`].
    pub fn mouse_cell(&self, pos: egui::Pos2) -> (u16, u16) {
        let col = (((pos.x - self.rect.min.x) / self.cw).floor() as i32 + 1)
            .clamp(1, self.cols as i32) as u16;
        let row = (((pos.y - self.rect.min.y) / self.rh).floor() as i32 + 1)
            .clamp(1, self.rows as i32) as u16;
        (col, row)
    }

    /// One cell's pixel rect.
    pub fn cell_rect(&self, row: usize, col: usize) -> egui::Rect {
        egui::Rect::from_min_size(
            egui::pos2(
                self.rect.min.x + col as f32 * self.cw,
                self.rect.min.y + row as f32 * self.rh,
            ),
            egui::vec2(self.cw, self.rh),
        )
    }

    /// The pixel rect covering columns `c0..=c1` of one row (selection /
    /// highlight spans).
    pub fn span_rect(&self, row: usize, c0: usize, c1: usize) -> egui::Rect {
        egui::Rect::from_min_size(
            egui::pos2(
                self.rect.min.x + c0 as f32 * self.cw,
                self.rect.min.y + row as f32 * self.rh,
            ),
            egui::vec2((c1 - c0 + 1) as f32 * self.cw, self.rh),
        )
    }
}

/// Scrollback thumb geometry: where the right-edge thumb sits for a viewport
/// of `rows` lines over `hist` lines of history, scrolled back `off` lines.
/// Pure math only — whether to SHOW the thumb stays with the caller.
pub(crate) fn thumb_rect(track: egui::Rect, rows: usize, hist: usize, off: i32) -> egui::Rect {
    let total = rows + hist;
    let track_h = track.height();
    let thumb_h = (track_h * rows as f32 / total as f32).max(16.0);
    let top_frac = (hist as i32 - off).max(0) as f32 / total as f32;
    let thumb_y = (track.min.y + track_h * top_frac).min(track.max.y - thumb_h);
    let w = 4.0;
    egui::Rect::from_min_size(egui::pos2(track.max.x - w, thumb_y), egui::vec2(w, thumb_h))
}

/// The caret's pixel rect within its cell: Beam (insert mode) and Underline
/// are thin 2px bars; Block and anything else fill the cell.
pub(crate) fn caret_rect(cell: egui::Rect, shape: CursorShape) -> egui::Rect {
    match shape {
        CursorShape::Beam => egui::Rect::from_min_size(cell.min, egui::vec2(2.0, cell.height())),
        CursorShape::Underline => egui::Rect::from_min_size(
            egui::pos2(cell.min.x, cell.max.y - 2.0),
            egui::vec2(cell.width(), 2.0),
        ),
        _ => cell,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> CellMetrics {
        // origin (10, 20), 8x16 cells, 4 cols x 3 rows
        CellMetrics::new(
            egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(32.0, 48.0)),
            8.0,
            16.0,
            4,
            3,
        )
    }

    #[test]
    fn cell_at_floors_to_the_containing_cell() {
        let m = metrics();
        assert_eq!(m.cell_at(egui::pos2(10.0, 20.0)), (0, 0));
        assert_eq!(m.cell_at(egui::pos2(17.9, 35.9)), (0, 0)); // just inside cell 0,0
        assert_eq!(m.cell_at(egui::pos2(18.0, 36.0)), (1, 1)); // first px of cell 1,1
        assert_eq!(m.cell_at(egui::pos2(41.9, 67.9)), (2, 3)); // last cell
    }

    #[test]
    fn cell_at_clamps_outside_the_pane_to_the_nearest_cell() {
        let m = metrics();
        assert_eq!(m.cell_at(egui::pos2(-100.0, -100.0)), (0, 0));
        assert_eq!(m.cell_at(egui::pos2(1000.0, 1000.0)), (2, 3));
    }

    /// The two pointer→cell readings must agree: mouse reporting is exactly
    /// the selection cell shifted to 1-based, in (col, row) protocol order.
    #[test]
    fn mouse_cell_is_cell_at_plus_one_in_col_row_order() {
        let m = metrics();
        for pos in [
            egui::pos2(10.0, 20.0),
            egui::pos2(25.0, 55.0),
            egui::pos2(41.9, 67.9),
            egui::pos2(-5.0, 999.0), // clamped on both axes
            egui::pos2(999.0, -5.0),
        ] {
            let (row, col) = m.cell_at(pos);
            assert_eq!(
                m.mouse_cell(pos),
                (col as u16 + 1, row as u16 + 1),
                "disagreement at {pos:?}"
            );
        }
    }

    #[test]
    fn span_rect_of_one_cell_matches_cell_rect() {
        let m = metrics();
        assert_eq!(m.span_rect(1, 2, 2), m.cell_rect(1, 2));
    }

    #[test]
    fn span_rect_covers_the_inclusive_column_range() {
        let m = metrics();
        let r = m.span_rect(2, 1, 3);
        assert_eq!(r.min, egui::pos2(10.0 + 8.0, 20.0 + 32.0));
        assert_eq!(r.width(), 3.0 * 8.0); // cols 1..=3
        assert_eq!(r.height(), 16.0);
    }

    #[test]
    fn thumb_is_proportional_and_right_aligned() {
        let track = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 400.0));
        // 40 viewport rows over 40 history rows: thumb is half the track.
        let r = thumb_rect(track, 40, 40, 0);
        assert_eq!(r.height(), 200.0);
        assert_eq!(r.max.x, 200.0);
        assert_eq!(r.width(), 4.0);
        // off == 0 (live prompt) → thumb sits at the bottom.
        assert_eq!(r.max.y, 400.0);
    }

    #[test]
    fn thumb_moves_to_the_top_when_fully_scrolled_back() {
        let track = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 400.0));
        let r = thumb_rect(track, 40, 40, 40); // off == hist
        assert_eq!(r.min.y, 0.0);
    }

    #[test]
    fn thumb_never_shrinks_below_the_grab_minimum() {
        let track = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 400.0));
        let r = thumb_rect(track, 10, 10_000, 0);
        assert_eq!(r.height(), 16.0);
        // and the min-height thumb still clamps inside the track
        assert!(r.max.y <= 400.0);
    }

    #[test]
    fn caret_rect_honors_the_requested_shape() {
        let cell = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(8.0, 16.0));
        let beam = caret_rect(cell, CursorShape::Beam);
        assert_eq!(beam.min, cell.min);
        assert_eq!((beam.width(), beam.height()), (2.0, 16.0));
        let under = caret_rect(cell, CursorShape::Underline);
        assert_eq!(under.min, egui::pos2(10.0, 34.0));
        assert_eq!((under.width(), under.height()), (8.0, 2.0));
        assert_eq!(caret_rect(cell, CursorShape::Block), cell);
    }
}
