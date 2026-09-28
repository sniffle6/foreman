//! Clickable links in a terminal pane: OSC 8 hyperlinks and plain URLs.
//!
//! Pure grid reads — no egui, no PTY — so the whole detection path is pinned
//! by `Term<VoidListener>` fixtures. `Session::show` asks [`link_at`] what is
//! under the pointer, underlines [`viewport_spans`], and opens the URI on
//! Ctrl+Click. Doc: docs/terminal-links.md.
//!
//! Two sources, OSC 8 first:
//! - **OSC 8** (`ESC ] 8 ; params ; URI ST`): alacritty stores the link on
//!   each cell (`Cell::hyperlink`). The span is the contiguous run of cells
//!   carrying the *same* link (same id + URI).
//! - **Plain URLs**: a scan of the logical line (rows joined by alacritty's
//!   `WRAPLINE` soft-wrap flag) for `scheme://…` runs.
//!
//! Only [`openable`] schemes are ever returned, so "underlined" always means
//! "Ctrl+Click will open it". `file:` is deliberately excluded: the OS shell
//! would *execute* a `file:///…/x.exe` link, and OSC 8 lets any program
//! print text that hides its real target.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::{Flags, Hyperlink};

/// How far a logical line is followed through soft wraps in each direction.
/// Bounds the per-hover cost on a pathological wall of wrapped text.
const MAX_WRAP_ROWS: i32 = 32;

/// Plain-text schemes the scanner recognises (all [`openable`]).
const SCHEMES: &[&str] = &["https://", "http://", "ftp://", "mailto:"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    /// OSC 8 — the visible text may differ from `uri`.
    Osc8,
    /// A URL detected in plain text — the text *is* the URI.
    Plain,
}

/// A link span in **buffer** coordinates (`Line` may be negative =
/// scrollback). `start..=end` in grid reading order; `end` includes the
/// spacer of a trailing wide glyph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub uri: String,
    pub kind: LinkKind,
    pub start: Point,
    pub end: Point,
}

/// Schemes foreman will hand to the OS browser opener.
pub fn openable(uri: &str) -> bool {
    let lower = uri.trim_start().to_ascii_lowercase();
    ["http://", "https://", "ftp://", "mailto:"]
        .iter()
        .any(|s| lower.starts_with(s) && lower.len() > s.len())
}

/// The openable link covering buffer `point`, if any.
pub fn link_at<L: EventListener>(term: &Term<L>, point: Point) -> Option<Link> {
    let grid = term.grid();
    let cols = grid.columns();
    let (top, bottom) = (grid.topmost_line(), grid.bottommost_line());
    if cols == 0 || point.line < top || point.line > bottom || point.column.0 >= cols {
        return None;
    }
    let last_col = Column(cols - 1);
    let wraps = |l: Line| grid[l][last_col].flags.contains(Flags::WRAPLINE);

    // The logical line: follow soft wraps up and down from the pointer row.
    let mut first = point.line;
    while first > top && point.line.0 - first.0 < MAX_WRAP_ROWS && wraps(first - 1) {
        first -= 1;
    }
    let mut last = point.line;
    while last < bottom && last.0 - point.line.0 < MAX_WRAP_ROWS && wraps(last) {
        last += 1;
    }

    // One entry per visible glyph (spacers skipped, like every text walk).
    let mut chars: Vec<char> = Vec::new();
    let mut points: Vec<Point> = Vec::new();
    let mut links: Vec<Option<Hyperlink>> = Vec::new();
    for l in first.0..=last.0 {
        let row = &grid[Line(l)];
        for c in 0..cols {
            let cell = &row[Column(c)];
            if crate::input::CellWide::is_wide_spacer(cell.flags) {
                continue;
            }
            chars.push(cell.c);
            points.push(Point::new(Line(l), Column(c)));
            links.push(cell.hyperlink());
        }
    }
    // Glyph under the pointer: the last one starting at or before it (a
    // spacer maps back onto its wide base).
    let idx = points.partition_point(|p| *p <= point).checked_sub(1)?;

    // A wide glyph's spacer belongs to the span it ends.
    let widen = |p: Point| {
        if grid[p.line][p.column].flags.contains(Flags::WIDE_CHAR) && p.column < last_col {
            Point::new(p.line, p.column + 1)
        } else {
            p
        }
    };

    if let Some(h) = &links[idx] {
        if !openable(h.uri()) {
            return None;
        }
        let same = |i: usize| links[i].as_ref() == Some(h);
        let mut a = idx;
        while a > 0 && same(a - 1) {
            a -= 1;
        }
        let mut b = idx;
        while b + 1 < links.len() && same(b + 1) {
            b += 1;
        }
        return Some(Link {
            uri: h.uri().to_string(),
            kind: LinkKind::Osc8,
            start: points[a],
            end: widen(points[b]),
        });
    }

    let (a, b) = find_urls(&chars)
        .into_iter()
        .find(|&(a, b)| (a..b).contains(&idx))?;
    Some(Link {
        uri: chars[a..b].iter().collect(),
        kind: LinkKind::Plain,
        start: points[a],
        end: widen(points[b - 1]),
    })
}

/// Characters that can continue a plain URL. Whitespace, controls, the
/// RFC 3986 "unwise" delimiters and box-drawing/block glyphs (TUI frames sit
/// flush against text) all end it.
fn is_url_char(c: char) -> bool {
    !c.is_whitespace()
        && !c.is_control()
        && !matches!(
            c,
            '<' | '>' | '"' | '`' | '{' | '}' | '^' | '\\' | '|' | '⟨' | '⟩'
        )
        && !('\u{2500}'..='\u{259F}').contains(&c)
}

/// Every plain URL in `text`, as `[start, end)` char-index ranges.
pub fn find_urls(text: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let boundary = i == 0 || !text[i - 1].is_alphanumeric();
        let scheme = SCHEMES.iter().find(|s| {
            boundary
                && s.chars().count() <= text.len() - i
                && s.chars()
                    .zip(&text[i..])
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
        });
        let Some(scheme) = scheme else {
            i += 1;
            continue;
        };
        let body = i + scheme.len();
        let mut end = body;
        while end < text.len() && is_url_char(text[end]) {
            end += 1;
        }
        end = trim_trailing(&text[i..end]) + i;
        if end > body {
            out.push((i, end));
            i = end;
        } else {
            i = body;
        }
    }
    out
}

/// Length of `url` once trailing prose punctuation and unbalanced closing
/// brackets are dropped: `(see https://x.io/a_(b)).` keeps `…/a_(b)`.
fn trim_trailing(url: &[char]) -> usize {
    let mut n = url.len();
    while n > 0 {
        let c = url[n - 1];
        let drop = match c {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' => true,
            ')' | ']' => {
                let open = if c == ')' { '(' } else { '[' };
                let opens = url[..n].iter().filter(|&&x| x == open).count();
                let closes = url[..n].iter().filter(|&&x| x == c).count();
                closes > opens
            }
            _ => false,
        };
        if !drop {
            break;
        }
        n -= 1;
    }
    n
}

/// The link's on-screen pieces as `(viewport_row, c0, c1)` inclusive column
/// ranges, culled to the visible viewport.
pub fn viewport_spans(
    link: &Link,
    display_offset: usize,
    screen_lines: usize,
    cols: usize,
) -> Vec<(usize, usize, usize)> {
    if cols == 0 {
        return Vec::new();
    }
    (link.start.line.0..=link.end.line.0)
        .filter_map(|l| {
            let row = l + display_offset as i32;
            if row < 0 || row as usize >= screen_lines {
                return None;
            }
            let c0 = if l == link.start.line.0 {
                link.start.column.0
            } else {
                0
            };
            let c1 = if l == link.end.line.0 {
                link.end.column.0
            } else {
                cols - 1
            };
            Some((row as usize, c0.min(cols - 1), c1.min(cols - 1)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
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

    fn at(line: i32, col: usize) -> Point {
        Point::new(Line(line), Column(col))
    }

    fn urls(s: &str) -> Vec<String> {
        let chars: Vec<char> = s.chars().collect();
        find_urls(&chars)
            .into_iter()
            .map(|(a, b)| chars[a..b].iter().collect())
            .collect()
    }

    #[test]
    fn find_urls_trims_prose_punctuation_and_unbalanced_brackets() {
        assert_eq!(urls("see https://a.io/x."), ["https://a.io/x"]);
        assert_eq!(urls("(https://a.io/x)"), ["https://a.io/x"]);
        assert_eq!(
            urls("https://en.wiki/Foo_(bar)"),
            ["https://en.wiki/Foo_(bar)"]
        );
        assert_eq!(urls("'http://a.io/q?x=1&y=2',"), ["http://a.io/q?x=1&y=2"]);
        assert_eq!(urls("<https://a.io>"), ["https://a.io"]);
    }

    #[test]
    fn find_urls_needs_a_word_boundary_and_a_body() {
        assert!(urls("xhttps://a.io").is_empty());
        assert!(urls("https://").is_empty());
        assert!(urls("https://.").is_empty());
        assert!(urls("file:///C:/evil.exe").is_empty());
        assert_eq!(urls("HTTPS://A.IO"), ["HTTPS://A.IO"]);
    }

    #[test]
    fn find_urls_stops_at_box_drawing_and_finds_several() {
        assert_eq!(urls("│https://a.io│"), ["https://a.io"]);
        assert_eq!(
            urls("http://a.io and mailto:me@x.io"),
            ["http://a.io", "mailto:me@x.io"]
        );
    }

    #[test]
    fn openable_allows_web_schemes_only() {
        assert!(openable("https://a.io"));
        assert!(openable("MAILTO:me@x.io"));
        assert!(!openable("file:///C:/Windows/notepad.exe"));
        assert!(!openable("javascript:alert(1)"));
        assert!(!openable("https://"));
    }

    #[test]
    fn plain_url_is_found_from_any_cell_inside_it() {
        let term = term_with(b"go https://a.io/x now", 40, 3);
        let want = Link {
            uri: "https://a.io/x".into(),
            kind: LinkKind::Plain,
            start: at(0, 3),
            end: at(0, 16),
        };
        assert_eq!(link_at(&term, at(0, 3)).as_ref(), Some(&want));
        assert_eq!(link_at(&term, at(0, 16)).as_ref(), Some(&want));
        assert_eq!(link_at(&term, at(0, 2)), None);
        assert_eq!(link_at(&term, at(0, 17)), None);
        assert_eq!(link_at(&term, at(1, 0)), None);
    }

    #[test]
    fn plain_url_follows_soft_wrap_across_rows() {
        // 10 columns: "https://ab" | "c.io/xyz"
        let term = term_with(b"https://abc.io/xyz", 10, 3);
        let link = link_at(&term, at(1, 2)).expect("wrapped url");
        assert_eq!(link.uri, "https://abc.io/xyz");
        assert_eq!((link.start, link.end), (at(0, 0), at(1, 7)));
        assert_eq!(viewport_spans(&link, 0, 3, 10), [(0, 0, 9), (1, 0, 7)]);
    }

    #[test]
    fn hard_newline_breaks_the_logical_line() {
        let term = term_with(b"https://ab\r\nc.io", 10, 3);
        assert_eq!(link_at(&term, at(0, 0)).unwrap().uri, "https://ab");
        assert_eq!(link_at(&term, at(1, 0)), None);
    }

    #[test]
    fn osc8_link_spans_its_text_and_carries_the_hidden_uri() {
        let term = term_with(
            b"a \x1b]8;;https://real.example/\x1b\\click me\x1b]8;;\x1b\\ b",
            40,
            2,
        );
        let link = link_at(&term, at(0, 5)).expect("osc8");
        assert_eq!(link.kind, LinkKind::Osc8);
        assert_eq!(link.uri, "https://real.example/");
        assert_eq!((link.start, link.end), (at(0, 2), at(0, 9)));
        assert_eq!(link_at(&term, at(0, 10)), None);
    }

    #[test]
    fn osc8_with_unopenable_scheme_is_not_a_link() {
        let term = term_with(b"\x1b]8;;file:///C:/x.exe\x1b\\run\x1b]8;;\x1b\\", 20, 2);
        assert_eq!(link_at(&term, at(0, 1)), None);
    }

    #[test]
    fn wide_glyph_at_url_end_includes_its_spacer() {
        let term = term_with("https://a.io/日".as_bytes(), 40, 2);
        let link = link_at(&term, at(0, 14)).expect("hover on spacer");
        assert_eq!(link.uri, "https://a.io/日");
        assert_eq!(link.end, at(0, 14));
    }

    #[test]
    fn viewport_spans_cull_rows_scrolled_out_of_view() {
        let link = Link {
            uri: "https://x".into(),
            kind: LinkKind::Plain,
            start: at(-2, 5),
            end: at(0, 3),
        };
        // Scrolled back 1, one screen line: -2 → row -1 and 0 → row 1 are culled.
        assert_eq!(viewport_spans(&link, 1, 1, 10), [(0, 0, 9)]);
        assert_eq!(
            viewport_spans(&link, 2, 3, 10),
            [(0, 5, 9), (1, 0, 9), (2, 0, 3)]
        );
    }
}
