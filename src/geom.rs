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

    pub fn cell_right_half(&self, pos: egui::Pos2) -> bool {
        let (_, col) = self.cell_at(pos);
        pos.x >= self.rect.min.x + (col as f32 + 0.5) * self.cw
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

/// Axis a scrollbar travels along. The cross-axis edge is always the rect's
/// trailing edge: right for vertical bars, bottom for horizontal bars.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub(crate) enum ScrollAxis {
    Horizontal,
    Vertical,
}

impl ScrollAxis {
    pub(crate) fn extent(self, rect: egui::Rect) -> f32 {
        match self {
            Self::Horizontal => rect.width(),
            Self::Vertical => rect.height(),
        }
    }

    pub(crate) fn main_min(self, rect: egui::Rect) -> f32 {
        match self {
            Self::Horizontal => rect.min.x,
            Self::Vertical => rect.min.y,
        }
    }

    pub(crate) fn main_pos(self, pos: egui::Pos2) -> f32 {
        match self {
            Self::Horizontal => pos.x,
            Self::Vertical => pos.y,
        }
    }
}

/// Painted cross-axis width of a scrollbar thumb at rest.
const THUMB_W: f32 = 4.0;
/// Painted cross-axis width while the pointer is in the track band, or during a drag — so
/// it reads as a control you can take hold of rather than a position readout.
const THUMB_HOT_W: f32 = 8.0;
/// How far the thumb keeps clear of the pane's trailing cross-axis edge.
///
/// The window manager puts an invisible resize hit-zone [`crate::wm::RESIZE_BAND`]
/// wide along every edge, and registers it with `ui.interact` AFTER the content,
/// so it wins the hover. A thumb drawn flush to the edge therefore lives inside
/// that zone: hovering it hovers the resize handle instead, the pane's own
/// response stops reporting `hovered()`, and the thumb hides exactly when you
/// reach for it. Deriving the inset from the band keeps the two from drifting.
const THUMB_EDGE_INSET: f32 = crate::wm::RESIZE_BAND;
/// Grab width. A 4px target is not hittable with a mouse, so the interactive
/// zone reaches inward from the edge further than the bar the user sees.
const THUMB_HIT_W: f32 = 14.0;
/// Floor on thumb length so deep content still leaves something to grab.
const THUMB_MIN_LEN: f32 = 16.0;

/// Thumb length and the distance its leading edge can travel along `axis`.
/// Shared by terminal scrollback and the Sessions panel so the two controls
/// cannot drift in proportional sizing or drag range.
fn scrollbar_metrics(
    track: egui::Rect,
    viewport_extent: f32,
    content_extent: f32,
    axis: ScrollAxis,
) -> (f32, f32) {
    let track_extent = axis.extent(track).max(0.0);
    let viewport_extent = viewport_extent.max(0.0);
    let content_extent = content_extent.max(viewport_extent).max(1.0);
    let thumb_extent = (track_extent * viewport_extent / content_extent)
        .max(THUMB_MIN_LEN)
        .min(track_extent);
    (thumb_extent, (track_extent - thumb_extent).max(0.0))
}

/// Axis-generic scrollbar thumb. `offset` is measured from the content's
/// leading edge (top or left) and clamped to the scrollable range.
pub(crate) fn scrollbar_thumb_rect(
    track: egui::Rect,
    viewport_extent: f32,
    content_extent: f32,
    offset: f32,
    axis: ScrollAxis,
) -> egui::Rect {
    let (thumb_extent, travel) = scrollbar_metrics(track, viewport_extent, content_extent, axis);
    let max_scroll = (content_extent - viewport_extent).max(0.0);
    let frac = if max_scroll <= 0.0 {
        0.0
    } else {
        offset.clamp(0.0, max_scroll) / max_scroll
    };
    let main = axis.main_min(track) + travel * frac;
    match axis {
        ScrollAxis::Vertical => {
            let right = track.max.x - THUMB_EDGE_INSET;
            egui::Rect::from_min_size(
                egui::pos2(right - THUMB_W, main),
                egui::vec2(THUMB_W, thumb_extent),
            )
        }
        ScrollAxis::Horizontal => {
            let bottom = track.max.y - THUMB_EDGE_INSET;
            egui::Rect::from_min_size(
                egui::pos2(main, bottom - THUMB_W),
                egui::vec2(thumb_extent, THUMB_W),
            )
        }
    }
}

/// Painted thumb while hovered or dragged, grown inward from the same edge.
pub(crate) fn scrollbar_hot_rect(bar: egui::Rect, axis: ScrollAxis) -> egui::Rect {
    match axis {
        ScrollAxis::Vertical => {
            egui::Rect::from_min_max(egui::pos2(bar.max.x - THUMB_HOT_W, bar.min.y), bar.max)
        }
        ScrollAxis::Horizontal => {
            egui::Rect::from_min_max(egui::pos2(bar.min.x, bar.max.y - THUMB_HOT_W), bar.max)
        }
    }
}

/// Grab zone for a thumb: same main-axis span, wider toward the content.
pub(crate) fn scrollbar_hit_rect(bar: egui::Rect, axis: ScrollAxis) -> egui::Rect {
    match axis {
        ScrollAxis::Vertical => {
            egui::Rect::from_min_max(egui::pos2(bar.max.x - THUMB_HIT_W, bar.min.y), bar.max)
        }
        ScrollAxis::Horizontal => {
            egui::Rect::from_min_max(egui::pos2(bar.min.x, bar.max.y - THUMB_HIT_W), bar.max)
        }
    }
}

/// Full track band used for hover, track clicks, and drag capture.
pub(crate) fn scrollbar_track_rect(track: egui::Rect, axis: ScrollAxis) -> egui::Rect {
    match axis {
        ScrollAxis::Vertical => {
            let right = track.max.x - THUMB_EDGE_INSET;
            egui::Rect::from_min_max(
                egui::pos2(right - THUMB_HIT_W, track.min.y),
                egui::pos2(right, track.max.y),
            )
        }
        ScrollAxis::Horizontal => {
            let bottom = track.max.y - THUMB_EDGE_INSET;
            egui::Rect::from_min_max(
                egui::pos2(track.min.x, bottom - THUMB_HIT_W),
                egui::pos2(track.max.x, bottom),
            )
        }
    }
}

/// Inverse of [`scrollbar_thumb_rect`]. `thumb_start` is the desired leading
/// edge of the thumb along `axis`; the returned content offset is clamped.
pub(crate) fn scrollbar_offset_for_thumb_start(
    track: egui::Rect,
    viewport_extent: f32,
    content_extent: f32,
    thumb_start: f32,
    axis: ScrollAxis,
) -> f32 {
    let max_scroll = (content_extent - viewport_extent).max(0.0);
    if max_scroll <= 0.0 {
        return 0.0;
    }
    let (_, travel) = scrollbar_metrics(track, viewport_extent, content_extent, axis);
    if travel <= 0.0 {
        return 0.0;
    }
    let frac = ((thumb_start - axis.main_min(track)) / travel).clamp(0.0, 1.0);
    frac * max_scroll
}

/// Scrollback thumb geometry: where the right-edge thumb sits for a viewport
/// of `rows` lines over `hist` lines of history, scrolled back `off` lines.
/// Pure math only — whether to SHOW the thumb stays with the caller.
///
/// `off == hist` (fully scrolled back) puts the top of the thumb at the top of
/// the track; `off == 0` (live prompt) puts its bottom at the bottom.
/// [`offset_for_thumb_top`] is the exact inverse.
pub(crate) fn thumb_rect(track: egui::Rect, rows: usize, hist: usize, off: i32) -> egui::Rect {
    let from_top = (hist as i32 - off).clamp(0, hist as i32) as f32;
    scrollbar_thumb_rect(
        track,
        rows as f32,
        (rows + hist) as f32,
        from_top,
        ScrollAxis::Vertical,
    )
}

/// The bar as painted while hovered or dragged: same rows and same right edge,
/// grown leftward. Pure so the widen is pinned without a GUI.
pub(crate) fn thumb_hot_rect(bar: egui::Rect) -> egui::Rect {
    scrollbar_hot_rect(bar, ScrollAxis::Vertical)
}

/// The thumb's grab zone: same rows as the painted bar, but wide enough to hit.
pub(crate) fn thumb_hit_rect(track: egui::Rect, rows: usize, hist: usize, off: i32) -> egui::Rect {
    scrollbar_hit_rect(thumb_rect(track, rows, hist, off), ScrollAxis::Vertical)
}

/// The full-height band on the right edge that counts as "the scrollbar" for
/// hit-testing — same width as [`thumb_hit_rect`], spanning the whole track.
/// A press in here but outside the thumb is a track click.
pub(crate) fn thumb_track_rect(track: egui::Rect) -> egui::Rect {
    scrollbar_track_rect(track, ScrollAxis::Vertical)
}

/// Inverse of [`thumb_rect`]: the `display_offset` that would place the thumb's
/// top at `thumb_top_y`. Clamped to `0..=hist`, so dragging past either end of
/// the track pins to the live bottom or the oldest scrollback rather than
/// running off. Pure math — the caller decides whether a drag is in progress.
pub(crate) fn offset_for_thumb_top(
    track: egui::Rect,
    rows: usize,
    hist: usize,
    thumb_top_y: f32,
) -> i32 {
    if hist == 0 {
        return 0;
    }
    let back = scrollbar_offset_for_thumb_start(
        track,
        rows as f32,
        (rows + hist) as f32,
        thumb_top_y,
        ScrollAxis::Vertical,
    )
    .round() as i32;
    (hist as i32 - back).clamp(0, hist as i32)
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
        // Right-aligned to the edge MINUS the resize band, not to the raw edge:
        // flush against it the wm's resize hit-zone eats the hover. See
        // thumb_and_its_hit_zone_clear_the_resize_band.
        assert_eq!(r.max.x, 200.0 - crate::wm::RESIZE_BAND);
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
    fn thumb_hit_rect_widens_the_grab_zone_only() {
        // 4px is not grabbable with a mouse. The hit zone widens leftward; the
        // painted bar (thumb_rect) is untouched, and both hug the same edge.
        let track = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 400.0));
        let bar = thumb_rect(track, 40, 40, 10);
        let hit = thumb_hit_rect(track, 40, 40, 10);
        assert_eq!((hit.min.y, hit.max.y), (bar.min.y, bar.max.y));
        assert_eq!(hit.max.x, bar.max.x, "same edge");
        assert!(hit.width() > bar.width(), "grab zone is wider than the bar");
        assert!(
            hit.contains(bar.center()),
            "the bar sits inside its own hit zone"
        );
    }

    #[test]
    fn thumb_and_its_hit_zone_clear_the_resize_band() {
        // The wm registers a RESIZE_BAND-wide edge hit-zone AFTER the content,
        // so it wins the hover. Anything of ours inside it is unhoverable and
        // ungrabbable: the thumb hid the instant you reached for it. Production
        // change that must fail this: dropping THUMB_EDGE_INSET back to 0.
        let track = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 400.0));
        let safe = track.max.x - crate::wm::RESIZE_BAND;
        for off in [0i32, 20, 40] {
            let bar = thumb_rect(track, 40, 40, off);
            let hit = thumb_hit_rect(track, 40, 40, off);
            assert!(bar.max.x <= safe, "bar inside the resize band (off={off})");
            assert!(
                hit.max.x <= safe,
                "grab zone inside the resize band (off={off})"
            );
            assert!(thumb_hot_rect(bar).max.x <= safe, "hot bar inside the band");
        }
        assert!(thumb_track_rect(track).max.x <= safe, "track band");
    }

    #[test]
    fn thumb_hot_rect_grows_leftward_from_the_same_edge() {
        let track = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 400.0));
        let bar = thumb_rect(track, 40, 40, 10);
        let hot = thumb_hot_rect(bar);
        assert_eq!((hot.min.y, hot.max.y), (bar.min.y, bar.max.y), "same rows");
        assert_eq!(hot.max.x, bar.max.x, "same right edge - it grows inward");
        assert!(hot.width() > bar.width());
    }

    #[test]
    fn generic_scrollbar_geometry_is_axis_symmetric() {
        let vertical_track =
            egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(200.0, 400.0));
        let vertical =
            scrollbar_thumb_rect(vertical_track, 200.0, 400.0, 0.0, ScrollAxis::Vertical);
        assert_eq!(vertical.height(), 200.0);
        assert_eq!(vertical.min.y, vertical_track.min.y);
        assert_eq!(
            vertical.max.x,
            vertical_track.max.x - crate::wm::RESIZE_BAND
        );

        let horizontal_track =
            egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(400.0, 200.0));
        let horizontal =
            scrollbar_thumb_rect(horizontal_track, 200.0, 400.0, 0.0, ScrollAxis::Horizontal);
        assert_eq!(horizontal.width(), 200.0);
        assert_eq!(horizontal.min.x, horizontal_track.min.x);
        assert_eq!(
            horizontal.max.y,
            horizontal_track.max.y - crate::wm::RESIZE_BAND
        );
        let horizontal_band = scrollbar_track_rect(horizontal_track, ScrollAxis::Horizontal);
        let horizontal_hit = scrollbar_hit_rect(horizontal, ScrollAxis::Horizontal);
        let horizontal_hot = scrollbar_hot_rect(horizontal, ScrollAxis::Horizontal);
        assert_eq!(horizontal_band.max.y, horizontal.max.y);
        assert_eq!(horizontal_hit.max.y, horizontal.max.y);
        assert_eq!(horizontal_hot.max.y, horizontal.max.y);
        assert_eq!(horizontal_band.height(), THUMB_HIT_W);
        assert_eq!(horizontal_hit.height(), THUMB_HIT_W);
        assert_eq!(horizontal_hot.height(), THUMB_HOT_W);
    }

    #[test]
    fn generic_scrollbar_drag_inverse_round_trips_on_both_axes() {
        let track = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(420.0, 310.0));
        for axis in [ScrollAxis::Vertical, ScrollAxis::Horizontal] {
            for offset in [0.0, 75.0, 300.0] {
                let bar = scrollbar_thumb_rect(track, 100.0, 400.0, offset, axis);
                let recovered =
                    scrollbar_offset_for_thumb_start(track, 100.0, 400.0, axis.main_min(bar), axis);
                assert!(
                    (recovered - offset).abs() < 0.01,
                    "axis={axis:?} offset={offset}"
                );
            }
        }
    }

    #[test]
    fn thumb_track_band_covers_the_thumb_at_every_offset() {
        // A track click is "in the band, outside the thumb", so the band must
        // contain the thumb wherever it sits or the two tests disagree.
        let track = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 400.0));
        let band = thumb_track_rect(track);
        assert_eq!((band.min.y, band.max.y), (track.min.y, track.max.y));
        for off in [0i32, 20, 40] {
            let hit = thumb_hit_rect(track, 40, 40, off);
            assert!(band.contains(hit.center()), "off={off}");
            assert_eq!(band.min.x, hit.min.x, "same width as the grab zone");
        }
    }

    #[test]
    fn thumb_offset_round_trips_through_thumb_rect() {
        // Dragging the thumb to where it already is must not move the view.
        let track = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 400.0));
        for (rows, hist) in [(40usize, 40usize), (24, 500), (10, 10_000)] {
            for off in [0i32, 1, hist as i32 / 3, hist as i32 - 1, hist as i32] {
                let y = thumb_rect(track, rows, hist, off).min.y;
                assert_eq!(
                    offset_for_thumb_top(track, rows, hist, y),
                    off,
                    "rows={rows} hist={hist} off={off}"
                );
            }
        }
    }

    #[test]
    fn thumb_offset_clamps_outside_the_track() {
        let track = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 400.0));
        // Dragged above the track = fully scrolled back; below = live bottom.
        assert_eq!(offset_for_thumb_top(track, 40, 40, -500.0), 40);
        assert_eq!(offset_for_thumb_top(track, 40, 40, 9999.0), 0);
    }

    #[test]
    fn thumb_drag_reaches_both_ends_with_a_min_height_thumb() {
        // The 16px floor shortens the thumb's travel. Mapping the drag over the
        // raw track height instead of the travel range leaves the last several
        // hundred lines of a deep buffer unreachable by dragging.
        let track = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 400.0));
        let (rows, hist) = (10usize, 10_000usize);
        let bottom = thumb_rect(track, rows, hist, 0).min.y;
        assert_eq!(offset_for_thumb_top(track, rows, hist, bottom), 0);
        assert_eq!(
            offset_for_thumb_top(track, rows, hist, track.min.y),
            hist as i32
        );
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

    #[test]
    fn cell_right_half_splits_a_cell_at_its_midpoint() {
        let m = metrics();
        // cell (0,0) spans x 10..18, midpoint 14
        assert!(!m.cell_right_half(egui::pos2(13.9, 28.0)));
        assert!(m.cell_right_half(egui::pos2(14.0, 28.0)));
    }

    #[test]
    fn cell_right_half_follows_the_clamped_cell_outside_the_pane() {
        let m = metrics();
        // left of the pane clamps to col 0 and reads as its left half
        assert!(!m.cell_right_half(egui::pos2(5.0, 28.0)));
        // right of the pane clamps to the last col and reads as its right half
        assert!(m.cell_right_half(egui::pos2(100.0, 28.0)));
    }
}
