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
use alacritty_terminal::term::cell::{Cell, Flags};
use eframe::egui;

use crate::caret::CursorDraw;
use crate::geom::CellMetrics;
use crate::terminal::{GlyphStyle, glyph_style};

/// One logical glyph locked to a grid cell column (not pixel-x from galley).
/// Spacer cells (`WIDE_CHAR_SPACER`) never produce a placement.
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphPlacement {
    pub row: usize,
    pub col: usize,
    pub ch: char,
    pub style: GlyphStyle,
    pub width_cells: u8, // 1 or 2
}

/// Site reserved for color-emoji stamp paint (populated; stamps later phase).
#[derive(Debug, Clone, PartialEq)]
pub struct EmojiSite {
    pub row: usize,
    pub col: usize,
    pub ch: char,
    pub width_cells: u8,
}

/// Pure frame paint plan: grid-locked glyphs + future emoji sites.
#[derive(Debug, Clone, PartialEq)]
pub struct PaintPlan {
    pub glyphs: Vec<GlyphPlacement>,
    pub emoji_sites: Vec<EmojiSite>,
}

/// True when `ch` has Unicode default emoji presentation (`Emoji_Presentation=Yes`).
///
/// Minimal single-scalar v1: range table only — no VS15/VS16, no ZWJ sequences.
/// Text-default symbols (e.g. ☁ U+2601) return false even if they are emoji.
pub fn is_default_emoji_presentation(ch: char) -> bool {
    let c = ch as u32;
    // Core emoji blocks (most codepoints here are EP=Yes).
    if (0x1F300..=0x1FAFF).contains(&c) {
        return true;
    }
    // Selected BMP / SMP singles and ranges with Emoji_Presentation=Yes.
    // Intentionally omits text-default emoji like U+2601 CLOUD.
    matches!(
        c,
        0x231A..=0x231B
            | 0x23E9..=0x23EC
            | 0x23F0
            | 0x23F3
            | 0x25FD..=0x25FE
            | 0x2614..=0x2615
            | 0x2648..=0x2653
            | 0x267F
            | 0x2693
            | 0x26A1
            | 0x26AA..=0x26AB
            | 0x26BD..=0x26BE
            | 0x26C4..=0x26C5
            | 0x26CE
            | 0x26D4
            | 0x26EA
            | 0x26F2..=0x26F3
            | 0x26F5
            | 0x26FA
            | 0x26FD
            | 0x2705
            | 0x270A..=0x270B
            | 0x2728
            | 0x274C
            | 0x274E
            | 0x2753..=0x2755
            | 0x2757
            | 0x2795..=0x2797
            | 0x27B0
            | 0x27BF
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
            | 0x1F004
            | 0x1F0CF
            | 0x1F18E
            | 0x1F191..=0x1F19A
            | 0x1F1E6..=0x1F1FF
    )
}

/// Walk the visible grid like [`text_rows`], but emit one [`GlyphPlacement`] per
/// logical cell at its grid column. Skips `WIDE_CHAR_SPACER`; wide chars get
/// `width_cells = 2`. Clamps to real grid bounds (same process-abort guard).
///
/// Also records [`EmojiSite`]s for wide default-emoji-presentation scalars
/// (stamp paint is a later phase — mono placement still always emitted).
pub fn plan_paint(grid: &Grid<Cell>, metrics: &CellMetrics) -> PaintPlan {
    let off = grid.display_offset() as i32;
    let ncols = metrics.cols().min(grid.columns());
    let nrows = metrics.rows().min(grid.screen_lines());
    let mut glyphs = Vec::with_capacity(nrows * ncols);
    let mut emoji_sites = Vec::new();
    for row in 0..nrows {
        for col in 0..ncols {
            let cell = &grid[Line(row as i32 - off)][Column(col)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let width_cells = if cell.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            let ch = if cell.c == '\0' { ' ' } else { cell.c };
            glyphs.push(GlyphPlacement {
                row,
                col,
                ch,
                style: glyph_style(cell.flags, cell.fg, cell.bg),
                width_cells,
            });
            // Stamp candidates: default emoji presentation + wide cell only.
            // Narrow (width 1) stays mono-only even if the range matches.
            if is_default_emoji_presentation(ch) && width_cells == 2 {
                emoji_sites.push(EmojiSite {
                    row,
                    col,
                    ch,
                    width_cells,
                });
            }
        }
    }
    PaintPlan {
        glyphs,
        emoji_sites,
    }
}

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

/// The per-cell text walk: batch consecutive cells sharing a GlyphStyle into
/// one StyleRun per run, one Vec<StyleRun> per row. The expensive, content-only
/// half of a frame — depends solely on grid content + geometry, so show()
/// caches the galley built from it and only re-walks when content/scroll/dims/
/// font change. Clamps to the grid's REAL size first (a stale index panics, and
/// a panic across the winit callback aborts the process).
pub fn text_rows(grid: &Grid<Cell>, metrics: &CellMetrics) -> Vec<Vec<StyleRun>> {
    let off = grid.display_offset() as i32;
    let ncols = metrics.cols().min(grid.columns());
    let nrows = metrics.rows().min(grid.screen_lines());
    let mut rows: Vec<Vec<StyleRun>> = Vec::with_capacity(nrows);
    for row in 0..nrows {
        let mut runs: Vec<StyleRun> = Vec::new();
        let mut run = String::new();
        let mut run_style = GlyphStyle {
            fg: crate::theme::FG,
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
    rows
}

/// The cheap, per-frame half: selection highlights (O(selected rows)), the
/// gated caret rect (O(1)), and the scrollback thumb (O(1)). None touch the
/// galley, so show() recomputes these every frame even on a cache hit — the
/// caret settles over time (Caret gate) and selection changes on drag.
pub struct Overlays {
    pub highlights: Vec<egui::Rect>,
    pub caret: Option<egui::Rect>,
    pub thumb: Option<egui::Rect>,
    pub scrolled_back: bool,
}

pub fn overlays(
    grid: &Grid<Cell>,
    metrics: &CellMetrics,
    selection: Option<SelRange>,
    cursor: CursorDraw,
) -> Overlays {
    let off = grid.display_offset() as i32;
    let ncols = metrics.cols().min(grid.columns());
    let nrows = metrics.rows().min(grid.screen_lines());

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

    let caret = match cursor {
        CursorDraw::At { line, col, shape } if line >= 0 && off == 0 => Some(
            crate::geom::caret_rect(metrics.cell_rect(line as usize, col), shape),
        ),
        _ => None,
    };

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

    Overlays {
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
        let rows = text_rows(term.grid(), &m);
        assert_eq!(rows.len(), 2, "must walk only the grid's 2 rows");
        for runs in &rows {
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
        let rows = text_rows(term.grid(), &m);
        assert_eq!(rows.len(), 2);
        for runs in &rows {
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
        let o = overlays(term.grid(), &m, Some(sel), CursorDraw::Hidden);
        // Rows 0 and 1 only — nothing at or beyond nrows (2).
        assert_eq!(o.highlights.len(), 2);
        let beyond = m.cell_rect(2, 0).min.y;
        assert!(
            o.highlights.iter().all(|r| r.min.y < beyond),
            "a highlight span landed at or beyond the grid's last row"
        );
    }

    // ---- text walk / run batching --------------------------------------------
    #[test]
    fn plan_batches_cells_into_style_runs() {
        // "ab" is plain, "cd" is underlined (ESC[4m): one run per style, in order.
        let term = term_with(b"ab\x1b[4mcd", 4, 1);
        let m = metrics(4, 1);
        let rows = text_rows(term.grid(), &m);
        assert_eq!(rows.len(), 1);
        let runs = &rows[0];
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
        let rows = text_rows(term.grid(), &m);
        let text: String = rows[0].iter().map(|r| r.text.as_str()).collect();
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
        let o = overlays(term.grid(), &m, None, cursor);
        assert!(o.caret.is_none(), "no caret while scrolled back");
        assert!(o.scrolled_back);
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
        let o = overlays(term.grid(), &m, None, cursor);
        let expect = crate::geom::caret_rect(m.cell_rect(1, 2), CursorShape::Block);
        assert_eq!(o.caret, Some(expect));
        assert!(!o.scrolled_back);
    }

    // ---- thumb ---------------------------------------------------------------
    #[test]
    fn thumb_none_without_history() {
        let term = term_with(b"hi", 4, 2);
        let m = metrics(4, 2);
        let o = overlays(term.grid(), &m, None, CursorDraw::Hidden);
        assert!(o.thumb.is_none());
        assert!(!o.scrolled_back);
    }

    #[test]
    fn thumb_some_with_history_and_bottom_is_not_scrolled_back() {
        // Enough lines to spill into history, but left at the live bottom
        // (off == 0): the thumb exists, yet scrolled_back is false.
        let term = term_with(b"1\r\n2\r\n3\r\n4\r\n5\r\n6", 4, 2);
        let m = metrics(4, 2);
        let o = overlays(term.grid(), &m, None, CursorDraw::Hidden);
        assert!(o.thumb.is_some());
        assert!(!o.scrolled_back, "at the live bottom, not scrolled back");
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
        let o = overlays(term.grid(), &m, Some(sel), CursorDraw::Hidden);
        assert_eq!(o.highlights.len(), 3);
        // first row: c0 = start col (2), c1 = last column (ncols-1 = 5)
        assert_eq!(o.highlights[0], m.span_rect(0, 2, 5));
        // middle row: full width 0..=5
        assert_eq!(o.highlights[1], m.span_rect(1, 0, 5));
        // last row: 0..=end col (3)
        assert_eq!(o.highlights[2], m.span_rect(2, 0, 3));
    }

    // ---- plan_paint: grid-locked glyph placements ----------------------------
    #[test]
    fn plan_paint_places_ascii_on_columns() {
        let term = term_with(b"ab", 4, 1);
        let m = metrics(4, 1);
        let plan = plan_paint(term.grid(), &m);
        let visible: Vec<_> = plan
            .glyphs
            .iter()
            .filter(|g| g.ch != ' ')
            .collect();
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].col, 0);
        assert_eq!(visible[0].ch, 'a');
        assert_eq!(visible[0].width_cells, 1);
        assert_eq!(visible[1].col, 1);
        assert_eq!(visible[1].ch, 'b');
    }

    #[test]
    fn plan_paint_wide_char_one_glyph_skips_spacer() {
        use alacritty_terminal::index::{Column, Line};
        use alacritty_terminal::term::cell::Flags;
        // 你好: two width-2 CJK glyphs at cols 0 and 2; spacer cells emit nothing.
        let term = term_with("你好".as_bytes(), 8, 1);
        let m = metrics(8, 1);
        let plan = plan_paint(term.grid(), &m);

        // Invariant: no GlyphPlacement may sit on a WIDE_CHAR_SPACER cell.
        for g in &plan.glyphs {
            let cell = &term.grid()[Line(0)][Column(g.col)];
            assert!(
                !cell.flags.contains(Flags::WIDE_CHAR_SPACER),
                "spacer cell must not appear as a GlyphPlacement: {g:?}"
            );
        }

        let wides: Vec<_> = plan.glyphs.iter().filter(|g| g.width_cells == 2).collect();
        assert_eq!(wides.len(), 2, "expected two wide CJK glyphs; got {:?}", plan.glyphs);
        assert_eq!((wides[0].col, wides[0].ch), (0, '你'));
        assert_eq!((wides[1].col, wides[1].ch), (2, '好'));
    }

    #[test]
    fn plan_paint_clamps_like_text_rows() {
        let term = term_with(b"ab\r\ncd", 4, 2);
        let m = CellMetrics::new(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(80.0, 160.0)),
            8.0,
            16.0,
            10,
            10,
        );
        let plan = plan_paint(term.grid(), &m);
        assert!(plan.glyphs.iter().all(|g| g.row < 2 && g.col < 4));
    }

    // ---- emoji_sites: default-presentation wide scalars ----------------------
    #[test]
    fn cucumber_is_default_emoji_presentation() {
        assert!(is_default_emoji_presentation('🥒'));
    }
    #[test]
    fn cloud_text_default_is_not_emoji_presentation() {
        assert!(!is_default_emoji_presentation('☁'));
        assert!(!is_default_emoji_presentation('A'));
    }
    #[test]
    fn plan_paint_emits_emoji_site_for_wide_emoji() {
        let term = term_with("🥒".as_bytes(), 8, 1);
        let m = metrics(8, 1);
        let plan = plan_paint(term.grid(), &m);
        // Hard asserts: alacritty marks default-presentation emoji wide (like CJK).
        // If this fails, fix term_with / feed path — do not soften.
        let g = plan
            .glyphs
            .iter()
            .find(|g| g.ch == '🥒')
            .expect("cucumber glyph placement");
        assert_eq!(g.width_cells, 2);
        assert_eq!(g.col, 0);
        assert_eq!(plan.emoji_sites.len(), 1);
        assert_eq!(plan.emoji_sites[0].ch, '🥒');
        assert_eq!(plan.emoji_sites[0].width_cells, 2);
        assert_eq!(plan.emoji_sites[0].col, 0);
    }
}
