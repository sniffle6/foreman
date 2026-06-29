//! The **Caret gate** (see CONTEXT.md): decide which cell the painted caret
//! rests at, given a stream of cursor observations and the user's recent typing.
//!
//! A full-screen TUI that doesn't bracket its frames in synchronized output (DEC
//! mode 2026) moves its cursor all over the screen mid-redraw. Foreman repaints
//! at a fixed rate and samples the grid wherever the parser happens to be, so a
//! naively-drawn caret strobes across the status line and chases startup
//! animations. This module de-jitters it.
//!
//! The gate is a small state machine over value types — no GUI, no terminal —
//! so the time-based derivation that has historically been the bug nest is
//! driven entirely through `observe`/`note_input` in unit tests by advancing an
//! injected `Instant`. `show()` does the trivial extraction from the grid model
//! and owns focus/scroll paint-gating and the egui drawing; the gate owns only
//! *where the caret rests* and *whether the app hid it*.

use std::time::{Duration, Instant};

use alacritty_terminal::vte::ansi::CursorShape;

/// How long the cursor must hold the same cell before the painted caret adopts
/// that position. Comfortably past intra-redraw chunk gaps (a frame or two of
/// scheduling jitter) yet short enough to feel instant between keystrokes.
const CURSOR_SETTLE: Duration = Duration::from_millis(50);

/// How recently the user must have typed for the caret to follow single-row
/// moves immediately. Long enough to cover an app's keystroke response (and to
/// span auto-repeat while a key is held), short enough that an autonomous
/// animation isn't mistaken for active editing.
const INPUT_GRACE: Duration = Duration::from_millis(150);

/// The live grid cursor, as the gate needs to see it.
#[derive(Debug, Clone, Copy)]
pub struct CursorModel {
    pub line: i32,
    pub col: usize,
    pub shape: CursorShape,
}

/// What the renderer should actually paint for the caret this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CursorDraw {
    Hidden,
    At {
        line: i32,
        col: usize,
        shape: CursorShape,
    },
}

/// Owns the de-jitter state: the painted (committed) position, the last cursor
/// cell observed and when it last moved (cursor-stability), and when the user
/// last typed (input-recency). All time is injected — the gate never reads the
/// clock itself.
pub struct CaretGate {
    /// The de-jittered caret position actually painted (line, col).
    committed: (i32, usize),
    /// The model cursor cell last observed, and when it last changed.
    cursor_seen: (i32, usize),
    cursor_moved_at: Instant,
    /// When the user last sent keyboard/paste input (`None` until first input).
    last_input_at: Option<Instant>,
}

impl CaretGate {
    pub fn new(now: Instant) -> Self {
        Self {
            committed: (0, 0),
            cursor_seen: (0, 0),
            cursor_moved_at: now,
            last_input_at: None,
        }
    }

    /// Record that the user just sent input (typing or paste). Call from the one
    /// spot in `read_input` where user-originated bytes are written to the PTY —
    /// never for scroll or machine-injected (dispatch/chat) bytes.
    pub fn note_input(&mut self, now: Instant) {
        self.last_input_at = Some(now);
    }

    /// Observe the live cursor and return the cell to paint. Advances the
    /// stability state and commits a new position when the policy adopts one.
    /// Runs every frame (even unfocused) so the state stays fresh.
    pub fn observe(&mut self, model: CursorModel, now: Instant) -> CursorDraw {
        // Track *cursor-position* stability (not output activity): reset the
        // timer only when the cursor actually moves to a new cell.
        if (model.line, model.col) != self.cursor_seen {
            self.cursor_seen = (model.line, model.col);
            self.cursor_moved_at = now;
        }
        let cursor_settled = now.duration_since(self.cursor_moved_at) >= CURSOR_SETTLE;
        let user_active = self
            .last_input_at
            .is_some_and(|t| now.duration_since(t) < INPUT_GRACE);

        let draw = cursor_to_draw(
            self.committed,
            model.line,
            model.col,
            model.shape,
            cursor_settled,
            user_active,
        );
        if let CursorDraw::At { line, col, .. } = draw {
            self.committed = (line, col);
        }
        draw
    }
}

/// The pure de-jitter decision (no time, no state) — the policy table.
///
/// The strobe always *teleports* the caret to a far row (status line / message
/// area) and back, whereas real line movement steps by a single row. So:
///   - the cursor has settled (held the same cell for a beat) -> adopt it
///     outright, even if other output is still streaming (a spinner, say);
///   - cursor still moving, within one row of committed, user actively editing
///     -> follow immediately (responsive typing, and backspacing across a
///     wrapped line keeps up even held down);
///   - cursor still moving, a far (>=2 row) jump -> hold the committed row to
///     swallow the strobe, until the cursor settles on its real resting row;
///   - cursor still moving, single-row, but no recent input -> also hold; that's
///     an autonomous animation (the startup "gloss" sweep), nothing to chase.
/// Visibility (`?25`) is honored immediately: hiding is a deliberate signal.
///
/// `cursor_settled` is *cursor-position* stability, NOT output quiescence: a
/// full-screen TUI can stream forever (animations) while its caret rests, and
/// the caret must still snap to that resting cell.
fn cursor_to_draw(
    committed: (i32, usize),
    model_line: i32,
    model_col: usize,
    model_shape: CursorShape,
    cursor_settled: bool,
    user_active: bool,
) -> CursorDraw {
    if model_shape == CursorShape::Hidden {
        return CursorDraw::Hidden;
    }
    // A far (>=2 row) jump while the cursor is still moving is the strobe: hold.
    // A single-row step is real line movement (wrap, backspace up, newline down)
    // — but only when the user is actively editing. An autonomous animation also
    // steps row-by-row with no keypress behind it, so the adjacent escape hatch
    // needs recent input.
    let adjacent = (model_line - committed.0).abs() <= 1;
    let (line, col) = if cursor_settled || (user_active && adjacent) {
        (model_line, model_col)
    } else {
        committed
    };
    CursorDraw::At {
        line,
        col,
        shape: model_shape,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }
    fn model(line: i32, col: usize) -> CursorModel {
        CursorModel {
            line,
            col,
            shape: CursorShape::Block,
        }
    }
    fn at(line: i32, col: usize) -> CursorDraw {
        CursorDraw::At {
            line,
            col,
            shape: CursorShape::Block,
        }
    }

    // ---- cursor_to_draw: the pure decision table -----------------------------
    #[test]
    fn decision_holds_far_jump_while_moving_even_when_typing() {
        // The status-line teleport (row 23) is held at the input row even while
        // the user is actively typing.
        let d = cursor_to_draw((5, 2), 23, 0, CursorShape::Block, false, true);
        assert_eq!(d, at(5, 2));
    }

    #[test]
    fn decision_tracks_col_on_same_row_while_typing() {
        let d = cursor_to_draw((5, 2), 5, 9, CursorShape::Block, false, true);
        assert_eq!(d, at(5, 9));
    }

    #[test]
    fn decision_follows_adjacent_row_move_while_typing() {
        // Backspacing across a wrapped line: caret up one row, followed now.
        let d = cursor_to_draw((5, 8), 4, 12, CursorShape::Block, false, true);
        assert_eq!(d, at(4, 12));
    }

    #[test]
    fn decision_holds_adjacent_autonomous_move() {
        // Single-row step with no recent input (the gloss sweep) — held.
        let d = cursor_to_draw((10, 4), 9, 1, CursorShape::Block, false, false);
        assert_eq!(d, at(10, 4));
    }

    #[test]
    fn decision_adopts_settled_cell_without_input() {
        let d = cursor_to_draw((5, 2), 23, 7, CursorShape::Block, true, false);
        assert_eq!(d, at(23, 7));
    }

    #[test]
    fn decision_honors_hide_immediately() {
        assert_eq!(
            cursor_to_draw((5, 2), 23, 0, CursorShape::Hidden, false, true),
            CursorDraw::Hidden
        );
    }

    // ---- observe: the time-based derivation (timeline tests) -----------------

    // Settle the gate at a starting cell so `committed` is known.
    fn settled_at(line: i32, col: usize, t0: Instant) -> CaretGate {
        let mut g = CaretGate::new(t0);
        g.observe(model(line, col), t0);
        let d = g.observe(
            model(line, col),
            t0 + ms(CURSOR_SETTLE.as_millis() as u64 + 10),
        );
        assert_eq!(d, at(line, col), "gate should settle at start cell");
        g
    }

    #[test]
    fn strobe_far_jump_is_held() {
        let t0 = Instant::now();
        let mut g = settled_at(5, 0, t0);
        // mid-redraw teleport to the status row, no user input
        let d = g.observe(model(23, 1), t0 + ms(61));
        assert_eq!(d, at(5, 0), "must never paint the status row");
    }

    #[test]
    fn backspace_across_line_follows_immediately() {
        let t0 = Instant::now();
        let mut g = settled_at(5, 8, t0);
        g.note_input(t0 + ms(100)); // user backspaces
        let d = g.observe(model(4, 20), t0 + ms(101)); // caret up one row, busy
        assert_eq!(d, at(4, 20), "adjacent + active -> follow now");
    }

    #[test]
    fn held_backspace_keeps_following_without_settle() {
        let t0 = Instant::now();
        let mut g = settled_at(5, 8, t0);
        g.note_input(t0 + ms(100));
        let d = g.observe(model(4, 20), t0 + ms(101));
        assert_eq!(d, at(4, 20));
        // auto-repeat: another keypress + another single-row step, still busy
        g.note_input(t0 + ms(140));
        let d = g.observe(model(3, 19), t0 + ms(141));
        assert_eq!(d, at(3, 19), "keeps up step by step, no settle wait");
    }

    #[test]
    fn gloss_sweep_is_held() {
        let t0 = Instant::now();
        let mut g = settled_at(2, 0, t0);
        // sweep to the adjacent row with no input behind it
        let d = g.observe(model(3, 5), t0 + ms(61));
        assert_eq!(d, at(2, 0), "autonomous adjacent move is not chased");
    }

    #[test]
    fn caret_adopts_resting_cell_after_animation_without_typing() {
        // The post-gloss freeze regression: cursor parks far away and stays;
        // once it has held that cell past the settle window, adopt it even with
        // no user input and even though output may still be streaming.
        let t0 = Instant::now();
        let mut g = settled_at(0, 0, t0);
        let d = g.observe(model(20, 4), t0 + ms(100)); // just moved -> far jump held
        assert_eq!(d, at(0, 0));
        let d = g.observe(model(20, 4), t0 + ms(160)); // held same cell 60ms -> adopt
        assert_eq!(d, at(20, 4), "must not stay frozen until the user types");
    }

    #[test]
    fn settle_threshold_is_50ms() {
        let t0 = Instant::now();
        let mut g = settled_at(1, 0, t0);
        g.observe(model(10, 2), t0 + ms(100)); // far move, moved_at = 100
        let d = g.observe(model(10, 2), t0 + ms(149)); // 49ms < 50 -> held
        assert_eq!(d, at(1, 0));
        let d = g.observe(model(10, 2), t0 + ms(150)); // exactly 50ms -> adopted
        assert_eq!(d, at(10, 2));
    }

    #[test]
    fn input_grace_threshold_is_150ms() {
        // Within grace: a single-row move is followed.
        let t0 = Instant::now();
        let mut g = settled_at(5, 0, t0);
        g.note_input(t0 + ms(100));
        let d = g.observe(model(6, 1), t0 + ms(249)); // 149ms since input -> active
        assert_eq!(d, at(6, 1));

        // Past grace, cursor still moving: the same step is held.
        let t1 = Instant::now();
        let mut g2 = settled_at(5, 0, t1);
        g2.note_input(t1 + ms(100));
        let d = g2.observe(model(6, 1), t1 + ms(250)); // exactly 150ms -> inactive
        assert_eq!(d, at(5, 0), "grace expired + still moving -> hold");
    }

    #[test]
    fn hide_is_honored_through_observe() {
        let t0 = Instant::now();
        let mut g = CaretGate::new(t0);
        let hidden = CursorModel {
            line: 5,
            col: 2,
            shape: CursorShape::Hidden,
        };
        assert_eq!(g.observe(hidden, t0 + ms(60)), CursorDraw::Hidden);
    }
}
