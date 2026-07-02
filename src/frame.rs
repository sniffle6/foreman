//! Frame plan — one frame's paint geometry and content for a Session's pane.
//!
//! This is the pure half of `Session::show`: given the grid model, the pane's
//! [`CellMetrics`], the current selection, and the [`CursorDraw`] the caret gate
//! already decided, it computes *what* to paint this frame — the styled text
//! runs, the selection highlight rects, the caret rect, and the scrollback thumb
//! rect. It decides nothing about *whether* to paint: focus (the caret's `active`
//! gate) and hover (the thumb's reveal) stay in `show()`, which replays this plan
//! into the egui painter with the frame's colors and corner radii.
//!
//! **Clamp rationale (the process-abort guard).** `pump()` advances the parser
//! the same frame, so the grid's real size can momentarily differ from the pane's
//! cached dims (alt-screen swap, reset, column-mode from a full-screen TUI). A
//! stale `grid[Line][Column]` index panics, and a panic across the winit callback
//! aborts the whole process. So `plan` clamps the walk to the grid's *actual*
//! bounds first, and the highlight builder clamps to those same bounds — the one
//! spot this arithmetic lives.

use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Cell;
use eframe::egui;

use crate::caret::CursorDraw;
use crate::geom::CellMetrics;
use crate::terminal::{GlyphStyle, glyph_style};

/// An ordered, inclusive selection span in **viewport** `(row, col)` coords — the
/// same space `CellMetrics::cell_at` returns. `start <= end`.
pub struct SelRange {
    pub start: (usize, usize),
    pub end: (usize, usize),
}

/// A maximal run of consecutive cells sharing one [`GlyphStyle`], as a single
/// string. `show()` turns each run into one egui `TextFormat` append.
pub struct StyleRun {
    pub text: String,
    pub style: GlyphStyle,
}

/// Everything one frame paints for a pane, minus the visibility decisions
/// (`active`/hover) and the paint style (colors, corner radii) that stay in
/// `show()`.
pub struct FramePlan {
    /// Outer index = grid row; each row is its cells batched into style runs.
    pub rows: Vec<Vec<StyleRun>>,
    /// One rect per selected row-span (`metrics.span_rect`).
    pub highlights: Vec<egui::Rect>,
    /// The caret rect — `Some` iff the gate said `At`, the line is on-screen
    /// (`line >= 0`), and the viewport is at the live bottom (`display_offset == 0`).
    pub caret: Option<egui::Rect>,
    /// The scrollback thumb rect — `Some` iff there is any history.
    pub thumb: Option<egui::Rect>,
    /// Whether the viewport is scrolled back into history (`display_offset > 0`).
    pub scrolled_back: bool,
}

pub fn plan(
    grid: &Grid<Cell>,
    metrics: &CellMetrics,
    selection: Option<SelRange>,
    cursor: CursorDraw,
) -> FramePlan {
    let off = grid.display_offset() as i32;
    // Clamp to the grid's REAL size first (see the module docs): a stale index
    // panics and a panic across the winit callback aborts the process. Both the
    // text walk and the highlight builder walk these clamped dims.
    let ncols = metrics.cols().min(grid.columns());
    let nrows = metrics.rows().min(grid.screen_lines());

    // Text: batch consecutive cells sharing a GlyphStyle into one StyleRun.
    let mut rows: Vec<Vec<StyleRun>> = Vec::with_capacity(nrows);
    for row in 0..nrows {
        let mut runs: Vec<StyleRun> = Vec::new();
        let mut run = String::new();
        let mut run_style = GlyphStyle {
            fg: crate::terminal::FG,
            bg: None,
            underline: false,
            strikethrough: false,
        };
        for col in 0..ncols {
            let cell = &grid[Line(row as i32 - off)][Column(col)];
            let style = glyph_style(cell.flags, cell.fg, cell.bg);
            if style != run_style {
                if !run.is_empty() {
                    runs.push(StyleRun {
                        text: std::mem::take(&mut run),
                        style: run_style,
                    });
                }
                run_style = style;
            }
            run.push(if cell.c == '\0' { ' ' } else { cell.c });
        }
        if !run.is_empty() {
            runs.push(StyleRun {
                text: run,
                style: run_style,
            });
        }
        rows.push(runs);
    }

    // Highlights: one span per selected row, clamped to the grid (rows to nrows,
    // columns to ncols) so a selection cached before a shrink can't ghost.
    let mut highlights = Vec::new();
    if let Some(sel) = &selection {
        for row in sel.start.0..=sel.end.0 {
            if row >= nrows {
                break;
            }
            let c0 = if row == sel.start.0 { sel.start.1 } else { 0 };
            let c1 = if row == sel.end.0 {
                sel.end.1
            } else {
                ncols.saturating_sub(1)
            }
            .min(ncols.saturating_sub(1));
            if c1 < c0 {
                continue;
            }
            highlights.push(metrics.span_rect(row, c0, c1));
        }
    }

    // Caret: only on the live viewport (off == 0), only when on-screen (line >= 0).
    let caret = match cursor {
        CursorDraw::At { line, col, shape } if line >= 0 && off == 0 => Some(
            crate::geom::caret_rect(metrics.cell_rect(line as usize, col), shape),
        ),
        _ => None,
    };

    // Thumb: exists whenever there is history; the cached row count matches today.
    let hist = grid.history_size();
    let thumb = if hist > 0 {
        Some(crate::geom::thumb_rect(
            metrics.rect(),
            metrics.rows(),
            hist,
            off,
        ))
    } else {
        None
    };

    FramePlan {
        rows,
        highlights,
        caret,
        thumb,
        scrolled_back: off > 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::{Dimensions, Scroll};
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::{CursorShape, Processor};

    struct Dims {
        cols: usize,
        rows: usize,
    }
    impl Dimensions for Dims {
        fn total_lines(&self) -> usize {
            self.rows
        }
        fn screen_lines(&self) -> usize {
            self.rows
        }
        fn columns(&self) -> usize {
            self.cols
        }
    }

    fn term_with(bytes: &[u8], cols: usize, rows: usize) -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &Dims { cols, rows }, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, bytes);
        term
    }

    /// Metrics with origin (0,0) and 8x16 cells, sized exactly to `cols`x`rows`.
    fn metrics(cols: usize, rows: usize) -> CellMetrics {
        CellMetrics::new(
            egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(cols as f32 * 8.0, rows as f32 * 16.0),
            ),
            8.0,
            16.0,
            cols,
            rows,
        )
    }

    // ---- clamp: the process-abort guard --------------------------------------
    #[test]
    fn plan_clamps_stale_metrics_to_grid_bounds() {
        // Grid is 4x2 but the metrics still claim a stale 10x10 (pump() shrank the
        // grid mid-frame). Indexing at 10x10 would panic and abort the process;
        // plan() must clamp to the grid's real bounds. Completing is the point.
        let term = term_with(b"ab\r\ncd", 4, 2);
        let m = CellMetrics::new(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(80.0, 160.0)),
            8.0,
            16.0,
            10,
            10,
        );
        let p = plan(term.grid(), &m, None, CursorDraw::Hidden);
        assert_eq!(p.rows.len(), 2, "must walk only the grid's 2 rows");
        for runs in &p.rows {
            let text: String = runs.iter().map(|r| r.text.as_str()).collect();
            assert!(
                text.chars().count() <= 4,
                "row walked past the grid's 4 columns: {text:?}"
            );
        }
    }

    #[test]
    fn plan_walks_only_cached_dims_when_grid_is_larger() {
        // The other direction: a 10x10 grid but the pane only laid out 4x2. plan()
        // paints exactly what the pane shows, no more.
        let term = term_with(b"", 10, 10);
        let m = metrics(4, 2);
        let p = plan(term.grid(), &m, None, CursorDraw::Hidden);
        assert_eq!(p.rows.len(), 2);
        for runs in &p.rows {
            let text: String = runs.iter().map(|r| r.text.as_str()).collect();
            assert!(text.chars().count() <= 4);
        }
    }

    #[test]
    fn plan_clamps_selection_beyond_grid() {
        // A selection cached before an alt-screen shrink can name rows past the
        // grid; the highlight builder must clamp to nrows and never index OOB.
        let term = term_with(b"hi\r\nyo", 4, 2);
        let m = metrics(4, 2);
        let sel = SelRange {
            start: (0, 0),
            end: (8, 3),
        };
        let p = plan(term.grid(), &m, Some(sel), CursorDraw::Hidden);
        // Rows 0 and 1 only — nothing at or beyond nrows (2).
        assert_eq!(p.highlights.len(), 2);
        let beyond = m.cell_rect(2, 0).min.y;
        assert!(
            p.highlights.iter().all(|r| r.min.y < beyond),
            "a highlight span landed at or beyond the grid's last row"
        );
    }

    // ---- text walk / run batching --------------------------------------------
    #[test]
    fn plan_batches_cells_into_style_runs() {
        // "ab" is plain, "cd" is underlined (ESC[4m): one run per style, in order.
        let term = term_with(b"ab\x1b[4mcd", 4, 1);
        let m = metrics(4, 1);
        let p = plan(term.grid(), &m, None, CursorDraw::Hidden);
        assert_eq!(p.rows.len(), 1);
        let runs = &p.rows[0];
        assert_eq!(runs.len(), 2, "a plain run then an underlined run");
        assert_eq!(runs[0].text, "ab");
        assert!(!runs[0].style.underline);
        assert_eq!(runs[1].text, "cd");
        assert!(runs[1].style.underline);
    }

    #[test]
    fn plan_renders_nul_cells_as_spaces() {
        // A fresh row is blank cells; the walk emits spaces, never a raw '\0'.
        let term = term_with(b"", 4, 1);
        let m = metrics(4, 1);
        let p = plan(term.grid(), &m, None, CursorDraw::Hidden);
        let text: String = p.rows[0].iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text, "    ");
        assert!(!text.contains('\0'));
    }

    // ---- caret ---------------------------------------------------------------
    #[test]
    fn caret_none_when_scrolled_back() {
        // Build history, then scroll back one line: the caret is suppressed (it
        // belongs only on the live viewport) and scrolled_back is true.
        let mut term = term_with(b"1\r\n2\r\n3\r\n4\r\n5\r\n6", 4, 2);
        term.scroll_display(Scroll::Delta(1));
        let m = metrics(4, 2);
        let cursor = CursorDraw::At {
            line: 1,
            col: 0,
            shape: CursorShape::Block,
        };
        let p = plan(term.grid(), &m, None, cursor);
        assert!(p.caret.is_none(), "no caret while scrolled back");
        assert!(p.scrolled_back);
    }

    #[test]
    fn caret_present_matches_geom_caret_rect() {
        // A visible block cursor at a known cell on the live viewport: plan's
        // caret rect is exactly geom's cell_rect -> caret_rect for that cell.
        let term = term_with(b"", 4, 2);
        let m = metrics(4, 2);
        let cursor = CursorDraw::At {
            line: 1,
            col: 2,
            shape: CursorShape::Block,
        };
        let p = plan(term.grid(), &m, None, cursor);
        let expect = crate::geom::caret_rect(m.cell_rect(1, 2), CursorShape::Block);
        assert_eq!(p.caret, Some(expect));
        assert!(!p.scrolled_back);
    }

    // ---- thumb ---------------------------------------------------------------
    #[test]
    fn thumb_none_without_history() {
        let term = term_with(b"hi", 4, 2);
        let m = metrics(4, 2);
        let p = plan(term.grid(), &m, None, CursorDraw::Hidden);
        assert!(p.thumb.is_none());
        assert!(!p.scrolled_back);
    }

    #[test]
    fn thumb_some_with_history_and_bottom_is_not_scrolled_back() {
        // Enough lines to spill into history, but left at the live bottom
        // (off == 0): the thumb exists, yet scrolled_back is false.
        let term = term_with(b"1\r\n2\r\n3\r\n4\r\n5\r\n6", 4, 2);
        let m = metrics(4, 2);
        let p = plan(term.grid(), &m, None, CursorDraw::Hidden);
        assert!(p.thumb.is_some());
        assert!(!p.scrolled_back, "at the live bottom, not scrolled back");
    }

    // ---- multi-row selection -------------------------------------------------
    #[test]
    fn multi_row_selection_spans_match_span_rect() {
        // A 3-row selection: the first row starts at its column and runs to the
        // last column; middle rows are full width; the last row runs to its end.
        let term = term_with(b"", 6, 4);
        let m = metrics(6, 4);
        let sel = SelRange {
            start: (0, 2),
            end: (2, 3),
        };
        let p = plan(term.grid(), &m, Some(sel), CursorDraw::Hidden);
        assert_eq!(p.highlights.len(), 3);
        // first row: c0 = start col (2), c1 = last column (ncols-1 = 5)
        assert_eq!(p.highlights[0], m.span_rect(0, 2, 5));
        // middle row: full width 0..=5
        assert_eq!(p.highlights[1], m.span_rect(1, 0, 5));
        // last row: 0..=end col (3)
        assert_eq!(p.highlights[2], m.span_rect(2, 0, 3));
    }
}
