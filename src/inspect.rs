//! Terminal inspection seam (terminal-inspection epic, Phase 1).
//!
//! Pure, GUI-free reads of a Session's emulated screen, plus key-name → PTY-byte
//! encoding for `foreman send`. Everything here is generic over the alacritty
//! `EventListener`, so it works on a live `Term<Listener>` in production AND a
//! `Term<VoidListener>` driven by fixed bytes in tests — no PTY, no window, no
//! control plane. The interface is the test surface. `control.rs` and the GUI are
//! thin adapters over these functions.
//!
//! Reads come in two flavours: plain text (`snapshot_text`) for the default
//! path, and per-cell attributes (`snapshot_cells`) for the `--attrs` opt-in.
//! Plus cursor, substring-match, and key encoding.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::CursorShape;
use eframe::egui;

/// A sub-rectangle of the viewport to snapshot. `None` everywhere = full viewport.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Region {
    pub row: usize,
    pub col: usize,
    pub rows: usize,
    pub cols: usize,
}

/// Where the cursor is and what shape it's drawn as.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CursorInfo {
    pub row: i32,
    pub col: usize,
    pub shape: String, // "block" | "beam" | "underline" | "hollow" | "hidden"
}

/// Per-cell rendering data for the `--attrs` opt-in: the glyph, resolved
/// foreground/background RGB, and the style flags. `bg` is `None` for the
/// default (transparent) background.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CellData {
    pub ch: char,
    pub fg: [u8; 3],
    pub bg: Option<[u8; 3]>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub inverse: bool,
    pub dim: bool,
    pub wide: bool,
}

/// The rendered viewport as plain text — one string per row, trailing spaces
/// trimmed, wide-char spacer cells skipped. `region` clamps to the grid.
pub fn snapshot_text<L: EventListener>(term: &Term<L>, region: Option<Region>) -> Vec<String> {
    let grid = term.grid();
    let off = grid.display_offset() as i32;
    let cols = grid.columns();
    let screen_rows = grid.screen_lines();
    // Clamp the region to the real grid so an out-of-range index can't panic.
    let (r0, r1, c0, c1) = match region {
        Some(r) => (
            r.row.min(screen_rows),
            (r.row + r.rows).min(screen_rows),
            r.col.min(cols),
            (r.col + r.cols).min(cols),
        ),
        None => (0, screen_rows, 0, cols),
    };
    let mut out = Vec::with_capacity(r1.saturating_sub(r0));
    for row in r0..r1 {
        let line = Line(row as i32 - off);
        let mut text = String::new();
        let mut col = c0;
        while col < c1 {
            let cell = &grid[line][Column(col)];
            // A wide (2-column) glyph lives in one cell; its trailing spacer is a
            // placeholder — skip it so the text isn't padded with a stray space.
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                col += 1;
                continue;
            }
            text.push(if cell.c == '\0' { ' ' } else { cell.c });
            col += 1;
        }
        out.push(text.trim_end().to_string());
    }
    out
}

/// The cursor's row/col and shape.
pub fn cursor_info<L: EventListener>(term: &Term<L>) -> CursorInfo {
    let c = term.renderable_content().cursor;
    let shape = match c.shape {
        CursorShape::Block => "block",
        CursorShape::Beam => "beam",
        CursorShape::Underline => "underline",
        CursorShape::HollowBlock => "hollow",
        CursorShape::Hidden => "hidden",
    };
    CursorInfo {
        row: c.point.line.0,
        col: c.point.column.0,
        shape: shape.to_string(),
    }
}

/// Does any row of the rendered viewport contain `pattern` (substring)?
pub fn grid_contains<L: EventListener>(term: &Term<L>, pattern: &str) -> bool {
    snapshot_text(term, None)
        .iter()
        .any(|l| l.contains(pattern))
}

/// Per-cell attribute snapshot for the `--attrs` opt-in. Walks the grid exactly
/// like [`snapshot_text`] (same region clamp, same `display_offset` row mapping,
/// same wide-char spacer skip) but emits one [`CellData`] per kept cell instead
/// of a flattened string. Colors resolve through the GUI palette
/// ([`crate::terminal::resolve`]) so attrs reads match what's painted.
pub fn snapshot_cells<L: EventListener>(
    term: &Term<L>,
    region: Option<Region>,
) -> Vec<Vec<CellData>> {
    let grid = term.grid();
    let off = grid.display_offset() as i32;
    let cols = grid.columns();
    let screen_rows = grid.screen_lines();
    let (r0, r1, c0, c1) = match region {
        Some(r) => (
            r.row.min(screen_rows),
            (r.row + r.rows).min(screen_rows),
            r.col.min(cols),
            (r.col + r.cols).min(cols),
        ),
        None => (0, screen_rows, 0, cols),
    };
    let mut out = Vec::with_capacity(r1.saturating_sub(r0));
    for row in r0..r1 {
        let line = Line(row as i32 - off);
        let mut row_cells = Vec::new();
        let mut col = c0;
        while col < c1 {
            let cell = &grid[line][Column(col)];
            // Skip the trailing spacer of a wide glyph (same rule as snapshot_text).
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                col += 1;
                continue;
            }
            let fg = crate::terminal::resolve(cell.fg).unwrap_or(crate::terminal::FG);
            let bg = crate::terminal::resolve(cell.bg).map(|c| [c.r(), c.g(), c.b()]);
            row_cells.push(CellData {
                ch: if cell.c == '\0' { ' ' } else { cell.c },
                fg: [fg.r(), fg.g(), fg.b()],
                bg,
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                underline: cell.flags.contains(Flags::UNDERLINE),
                strikethrough: cell.flags.contains(Flags::STRIKEOUT),
                inverse: cell.flags.contains(Flags::INVERSE),
                dim: cell.flags.contains(Flags::DIM),
                wide: cell.flags.contains(Flags::WIDE_CHAR),
            });
            col += 1;
        }
        out.push(row_cells);
    }
    out
}

/// Map a single letter to its egui `Key` (case-insensitive).
fn letter_key(c: char) -> Option<egui::Key> {
    use egui::Key::*;
    Some(match c.to_ascii_uppercase() {
        'A' => A,
        'B' => B,
        'C' => C,
        'D' => D,
        'E' => E,
        'F' => F,
        'G' => G,
        'H' => H,
        'I' => I,
        'J' => J,
        'K' => K,
        'L' => L,
        'M' => M,
        'N' => N,
        'O' => O,
        'P' => P,
        'Q' => Q,
        'R' => R,
        'S' => S,
        'T' => T,
        'U' => U,
        'V' => V,
        'W' => W,
        'X' => X,
        'Y' => Y,
        'Z' => Z,
        _ => return None,
    })
}

/// Map a key name from the `--keys` grammar to an egui `Key`.
fn key_from_name(name: &str) -> Option<egui::Key> {
    use egui::Key::*;
    Some(match name {
        "Enter" => Enter,
        "Tab" => Tab,
        "Esc" | "Escape" => Escape,
        "Backspace" => Backspace,
        "Space" => Space,
        "Up" => ArrowUp,
        "Down" => ArrowDown,
        "Left" => ArrowLeft,
        "Right" => ArrowRight,
        "Home" => Home,
        "End" => End,
        "PageUp" => PageUp,
        "PageDown" => PageDown,
        "Insert" | "Ins" => Insert,
        "Delete" | "Del" => Delete,
        "F1" => F1,
        "F2" => F2,
        "F3" => F3,
        "F4" => F4,
        "F5" => F5,
        "F6" => F6,
        "F7" => F7,
        "F8" => F8,
        "F9" => F9,
        "F10" => F10,
        "F11" => F11,
        "F12" => F12,
        _ if name.chars().count() == 1 => return letter_key(name.chars().next().unwrap()),
        _ => return None,
    })
}

/// Parse one `--keys` token (e.g. `Ctrl+Shift+F5`) into a key + modifiers.
fn parse_one_key(token: &str) -> Result<(egui::Key, egui::Modifiers), String> {
    let mut mods = egui::Modifiers::default();
    let mut rest = token;
    while let Some(idx) = rest.find('+') {
        let (m, tail) = rest.split_at(idx);
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "alt" | "meta" | "option" => mods.alt = true,
            "shift" => mods.shift = true,
            other => return Err(format!("unknown modifier: {other}")),
        }
        rest = &tail[1..]; // drop the '+'
    }
    let key = key_from_name(rest).ok_or_else(|| format!("unknown key: {rest}"))?;
    Ok((key, mods))
}

/// Encode a sequence of named key presses into PTY bytes, via the
/// `input::encode_key` seam (so `send --keys` and the live keyboard never
/// diverge). Names: `F1`..`F12`, `Up/Down/Left/Right`, `Home/End/PageUp/PageDown/
/// Insert/Delete`, `Enter/Tab/Esc/Backspace/Space`, single letters; with
/// `Ctrl+`/`Alt+`/`Shift+` prefixes. A bare printable letter (no Ctrl/Alt) has no
/// key-sequence — use `--text` for literal characters; it's an error here.
pub fn parse_keys(names: &[String], mode: TermMode) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for token in names {
        if token.is_empty() {
            continue;
        }
        let (key, mods) = parse_one_key(token)?;
        let bytes = crate::input::encode_key(key, mods, mode);
        if bytes.is_empty() {
            return Err(format!(
                "key '{token}' has no input sequence — use --text for literal characters"
            ));
        }
        out.extend_from_slice(&bytes);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::Processor;

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

    fn s(x: &str) -> String {
        x.to_string()
    }

    // ---- snapshot_text -------------------------------------------------------
    #[test]
    fn snapshot_text_returns_plain_rows() {
        let term = term_with(b"hello", 20, 3);
        let rows = snapshot_text(&term, None);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], "hello"); // trailing spaces trimmed
        assert_eq!(rows[1], "");
    }

    #[test]
    fn snapshot_text_skips_wide_char_spacer() {
        // A CJK glyph is width-2 (one WIDE_CHAR cell + one WIDE_CHAR_SPACER); the
        // output must be one char, not the char plus a stray space.
        let term = term_with("ab漢z".as_bytes(), 20, 1);
        assert_eq!(snapshot_text(&term, None)[0], "ab漢z");
    }

    #[test]
    fn snapshot_text_region_clamps_without_panic() {
        let term = term_with(b"row0\r\nrow1", 20, 3);
        // a region larger than the grid must clamp, not panic
        let rows = snapshot_text(
            &term,
            Some(Region {
                row: 0,
                col: 0,
                rows: 99,
                cols: 99,
            }),
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], "row0");
    }

    // ---- snapshot_cells ------------------------------------------------------
    #[test]
    fn snapshot_cells_plain_cell_has_no_flags() {
        let term = term_with(b"A", 20, 1);
        let grid = snapshot_cells(&term, None);
        assert_eq!(grid.len(), 1);
        let a = &grid[0][0];
        assert_eq!(a.ch, 'A');
        assert!(!a.bold);
        assert!(!a.italic);
        assert!(!a.underline);
        assert!(!a.strikethrough);
        assert!(!a.inverse);
        assert!(!a.dim);
        assert!(!a.wide);
    }

    #[test]
    fn snapshot_cells_reports_underline() {
        // ESC[4m turns underline on
        let term = term_with(b"\x1b[4mU", 20, 1);
        let grid = snapshot_cells(&term, None);
        let u = grid[0].iter().find(|c| c.ch == 'U').expect("U cell");
        assert!(u.underline);
        assert!(!u.inverse);
    }

    #[test]
    fn snapshot_cells_reports_inverse() {
        // ESC[7m turns inverse (reverse video) on
        let term = term_with(b"\x1b[7mI", 20, 1);
        let grid = snapshot_cells(&term, None);
        let i = grid[0].iter().find(|c| c.ch == 'I').expect("I cell");
        assert!(i.inverse);
        assert!(!i.underline);
    }

    #[test]
    fn snapshot_cells_region_clamps_without_panic() {
        let term = term_with(b"hi", 20, 3);
        let grid = snapshot_cells(
            &term,
            Some(Region {
                row: 0,
                col: 0,
                rows: 99,
                cols: 99,
            }),
        );
        assert_eq!(grid.len(), 3);
        assert_eq!(grid[0][0].ch, 'h');
    }

    #[test]
    fn snapshot_cells_skips_wide_char_spacer() {
        // A CJK glyph is one WIDE_CHAR cell + one WIDE_CHAR_SPACER; the spacer
        // must be dropped, so the wide glyph appears once with wide=true.
        let term = term_with("漢".as_bytes(), 20, 1);
        let grid = snapshot_cells(&term, None);
        let han = grid[0].iter().find(|c| c.ch == '漢').expect("wide glyph");
        assert!(han.wide);
    }

    // ---- cursor_info ---------------------------------------------------------
    #[test]
    fn cursor_info_reports_position_after_text() {
        let c = cursor_info(&term_with(b"abc", 20, 3));
        assert_eq!(c.row, 0);
        assert_eq!(c.col, 3); // sits just past "abc"
    }

    #[test]
    fn cursor_info_reports_shape_from_decscusr() {
        // ESC [ 6 SP q = steady bar (beam)
        assert_eq!(cursor_info(&term_with(b"\x1b[6 q", 20, 1)).shape, "beam");
    }

    // ---- grid_contains -------------------------------------------------------
    #[test]
    fn grid_contains_finds_substring() {
        let term = term_with(b"the PASS marker", 30, 2);
        assert!(grid_contains(&term, "PASS"));
        assert!(!grid_contains(&term, "FAIL"));
    }

    // ---- parse_keys ----------------------------------------------------------
    #[test]
    fn parse_keys_encodes_named_and_modified_keys() {
        let m = TermMode::empty();
        assert_eq!(parse_keys(&[s("F5")], m).unwrap(), b"\x1b[15~");
        assert_eq!(parse_keys(&[s("Up")], m).unwrap(), b"\x1b[A");
        assert_eq!(parse_keys(&[s("Enter")], m).unwrap(), vec![b'\r']);
        assert_eq!(parse_keys(&[s("Ctrl+C")], m).unwrap(), vec![0x03]);
        assert_eq!(parse_keys(&[s("Alt+b")], m).unwrap(), vec![0x1b, b'b']);
        assert_eq!(parse_keys(&[s("Ctrl+Right")], m).unwrap(), b"\x1b[1;5C");
    }

    #[test]
    fn parse_keys_honors_app_cursor_mode() {
        assert_eq!(
            parse_keys(&[s("Up")], TermMode::APP_CURSOR).unwrap(),
            b"\x1bOA"
        );
    }

    #[test]
    fn parse_keys_concatenates_a_sequence() {
        let out = parse_keys(&[s("Escape"), s("Enter")], TermMode::empty()).unwrap();
        assert_eq!(out, vec![0x1b, b'\r']);
    }

    #[test]
    fn parse_keys_rejects_unknown_key_and_bare_letter() {
        assert!(parse_keys(&[s("Splork")], TermMode::empty()).is_err());
        assert!(parse_keys(&[s("Ctrl+Splork")], TermMode::empty()).is_err());
        // a bare printable letter has no key-sequence — must error, point to --text
        assert!(parse_keys(&[s("a")], TermMode::empty()).is_err());
    }
}
