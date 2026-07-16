//! Caret drawing decision: what the painter should draw for the model cursor.
//!
//! History: this module used to house the "Caret gate" — a 50ms settle /
//! 150ms input-grace debounce that held the painted caret still while a
//! non-synchronized TUI teleported its cursor mid-redraw (the 2026-06 Codex
//! status-line strobe). Measured evidence retired it (2026-07-15, the
//! `caret_probe_claude_typing` probe in terminal.rs): modern TUIs (Claude
//! Code, Codex) bracket every redraw in DEC 2026 synchronized output, which
//! the parser applies atomically, and PSReadLine's per-keystroke redraws
//! arrive hide-bracketed (`?25l..?25h`) in single chunks — the model cursor
//! stream is already clean by the time the painter samples it. The gate's
//! holds, meanwhile, were the visible cost: a 50ms stale-cell hold on every
//! ≥2-row composer move and on any echo slower than the grace window. Every
//! surveyed terminal (Alacritty, WezTerm, Kitty, Windows Terminal, Ghostty)
//! paints the model cursor directly with zero position debouncing; foreman
//! now does the same. Full story: docs/cursor-rendering.md.

use alacritty_terminal::vte::ansi::CursorShape;

/// What the renderer should paint for the caret this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CursorDraw {
    Hidden,
    At {
        line: i32,
        col: usize,
        shape: CursorShape,
    },
}

/// The model cursor, as painted: `?25l` (hide) is honored; everything else is
/// drawn exactly where the grid model says the cursor is, every frame.
pub fn draw(line: i32, col: usize, shape: CursorShape) -> CursorDraw {
    if shape == CursorShape::Hidden {
        CursorDraw::Hidden
    } else {
        CursorDraw::At { line, col, shape }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_is_honored() {
        assert_eq!(draw(5, 2, CursorShape::Hidden), CursorDraw::Hidden);
    }

    #[test]
    fn visible_cursor_is_drawn_where_the_model_says() {
        assert_eq!(
            draw(5, 2, CursorShape::Beam),
            CursorDraw::At {
                line: 5,
                col: 2,
                shape: CursorShape::Beam
            }
        );
    }
}
