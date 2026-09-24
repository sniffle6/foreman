//! Pure unified-diff parsing into aligned side-by-side rows. No egui, no I/O:
//! everything the painter needs is computed here, once, on the worker.
use std::ops::Range;

/// `-U` value for diff reads, and so the effective line cap: a change more than
/// this many lines from the file start, or from the next change, cannot come
/// back as one whole-file hunk, which `parse` reports as `Notice::TooLarge`.
/// Must stay far below i32::MAX: git 2.39 emits overlapping hunks at `-U2147483647`.
pub(super) const CONTEXT_LINES: usize = 200_000;

#[derive(Debug, PartialEq)]
pub(super) enum Notice {
    Binary,
    TooLarge,
    Unchanged,
    Submodule,
}
impl Notice {
    pub(super) fn text(&self) -> &'static str {
        match self {
            Notice::Binary => "Binary file — not shown.",
            Notice::TooLarge => {
                "Diff too large to show (over 16 MiB, or changes over 200,000 lines apart)."
            }
            Notice::Unchanged => "No content changes.",
            Notice::Submodule => "Submodule change — not shown.",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Kind {
    Same,
    Removed,
    Added,
    Modified,
}
#[derive(Debug, PartialEq)]
pub(super) struct Cell {
    pub(super) line: u32,
    /// Display text: lossy UTF-8, trailing `\r` stripped, tabs expanded.
    pub(super) text: String,
    /// Byte range (char-aligned) of the differing middle, `Modified` rows only.
    pub(super) hot: Option<Range<usize>>,
    /// `hot` in chars (= columns), so painting never counts a long line.
    pub(super) hot_chars: Option<Range<usize>>,
    /// Char count (= columns) of `text`. Equal to `text.len()` iff ASCII.
    pub(super) chars: usize,
    pub(super) no_eol: bool,
}
#[derive(Debug, PartialEq)]
pub(super) struct Row {
    pub(super) kind: Kind,
    pub(super) old: Option<Cell>,
    pub(super) new: Option<Cell>,
}
#[derive(Debug, PartialEq)]
pub(super) struct Block {
    pub(super) rows: Range<usize>,
    pub(super) kind: Kind,
}
#[derive(Debug, Default, PartialEq)]
pub(super) struct Doc {
    pub(super) rows: Vec<Row>,
    pub(super) blocks: Vec<Block>,
    pub(super) max_cols: usize,
    pub(super) max_line: u32,
}
#[derive(Debug, PartialEq)]
pub(super) enum Diff {
    Doc(Doc),
    Notice(Notice),
}

const MALFORMED: &str = "Git returned a malformed diff";

pub(super) fn parse(bytes: &[u8]) -> Result<Diff, String> {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let mut lines = bytes.split(|b| *b == b'\n');
    let mut header = None;
    for line in lines.by_ref() {
        if line.starts_with(b"@@") {
            header = Some(parse_header(line)?);
            break;
        }
        if line.starts_with(b"Binary files ") || line.starts_with(b"GIT binary patch") {
            return Ok(Diff::Notice(Notice::Binary));
        }
        let gitlink = (line.starts_with(b"index ") && line.ends_with(b" 160000"))
            || line.ends_with(b"file mode 160000");
        if gitlink {
            return Ok(Diff::Notice(Notice::Submodule));
        }
    }
    let Some((old_start, old_count, new_start, new_count)) = header else {
        return Ok(Diff::Notice(Notice::Unchanged));
    };
    if old_start > 1 || new_start > 1 {
        return Ok(Diff::Notice(Notice::TooLarge));
    }
    let mut b = Builder::default();
    for line in lines {
        match line.first() {
            Some(b' ') => b.context(&line[1..]),
            None => b.context(b""),
            Some(b'-') => b.removed(&line[1..]),
            Some(b'+') => b.added(&line[1..]),
            Some(b'\\') => b.no_eol(),
            Some(b'@') => return Ok(Diff::Notice(Notice::TooLarge)),
            _ => return Err(MALFORMED.into()),
        }
    }
    b.flush();
    if b.old_no != old_count || b.new_no != new_count {
        return Ok(Diff::Notice(Notice::TooLarge));
    }
    b.doc.max_line = b.old_no.max(b.new_no);
    Ok(Diff::Doc(b.doc))
}

/// `@@ -a[,b] +c[,d] @@[ context]` → (a, b, c, d); a missing count means 1.
fn parse_header(line: &[u8]) -> Result<(u32, u32, u32, u32), String> {
    let text = String::from_utf8_lossy(line);
    let mut parts = text.split(' ');
    let (Some("@@"), Some(old), Some(new), Some("@@")) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(MALFORMED.into());
    };
    let range = |s: &str, sign: char| -> Option<(u32, u32)> {
        let s = s.strip_prefix(sign)?;
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let ((a, b), (c, d)) = range(old, '-').zip(range(new, '+')).ok_or(MALFORMED)?;
    Ok((a, b, c, d))
}

#[derive(Clone, Copy, Default, PartialEq)]
enum Last {
    #[default]
    None,
    Context,
    Removed,
    Added,
}
#[derive(Default)]
struct Builder {
    doc: Doc,
    /// Lines consumed so far on each side = the last line number used.
    old_no: u32,
    new_no: u32,
    removed: Vec<Cell>,
    added: Vec<Cell>,
    last: Last,
}
impl Builder {
    fn cell(&mut self, line: u32, raw: &[u8]) -> Cell {
        let (text, cols) = display(raw);
        self.doc.max_cols = self.doc.max_cols.max(cols);
        Cell {
            line,
            text,
            hot: None,
            hot_chars: None,
            chars: cols,
            no_eol: false,
        }
    }
    fn context(&mut self, raw: &[u8]) {
        self.flush();
        self.old_no = self.old_no.saturating_add(1);
        self.new_no = self.new_no.saturating_add(1);
        let old = self.cell(self.old_no, raw);
        let new = self.cell(self.new_no, raw);
        self.doc.rows.push(Row {
            kind: Kind::Same,
            old: Some(old),
            new: Some(new),
        });
        self.last = Last::Context;
    }
    fn removed(&mut self, raw: &[u8]) {
        // A '-' after '+' lines starts a new block.
        if !self.added.is_empty() {
            self.flush();
        }
        self.old_no = self.old_no.saturating_add(1);
        let cell = self.cell(self.old_no, raw);
        self.removed.push(cell);
        self.last = Last::Removed;
    }
    fn added(&mut self, raw: &[u8]) {
        self.new_no = self.new_no.saturating_add(1);
        let cell = self.cell(self.new_no, raw);
        self.added.push(cell);
        self.last = Last::Added;
    }
    fn no_eol(&mut self) {
        match self.last {
            Last::Removed => self.removed.last_mut().map(|c| c.no_eol = true),
            Last::Added => self.added.last_mut().map(|c| c.no_eol = true),
            Last::Context => self.doc.rows.last_mut().map(|r| {
                for c in [&mut r.old, &mut r.new].into_iter().flatten() {
                    c.no_eol = true;
                }
            }),
            Last::None => None,
        };
    }
    fn flush(&mut self) {
        if self.removed.is_empty() && self.added.is_empty() {
            return;
        }
        let kind = match (self.removed.is_empty(), self.added.is_empty()) {
            (false, false) => Kind::Modified,
            (false, true) => Kind::Removed,
            _ => Kind::Added,
        };
        let start = self.doc.rows.len();
        let n = self.removed.len().max(self.added.len());
        let mut old = std::mem::take(&mut self.removed).into_iter();
        let mut new = std::mem::take(&mut self.added).into_iter();
        for _ in 0..n {
            let (mut o, mut a) = (old.next(), new.next());
            let kind = match (&o, &a) {
                (Some(_), Some(_)) => Kind::Modified,
                (Some(_), None) => Kind::Removed,
                _ => Kind::Added,
            };
            if let (Some(o), Some(a)) = (&mut o, &mut a) {
                trim(o, a);
            }
            self.doc.rows.push(Row {
                kind,
                old: o,
                new: a,
            });
        }
        self.doc.blocks.push(Block {
            rows: start..self.doc.rows.len(),
            kind,
        });
    }
}

/// Lossy-decode, strip one trailing `\r`, expand tabs to 4-column stops.
/// Returns the text and its width in columns (chars).
fn display(raw: &[u8]) -> (String, usize) {
    let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    let mut col = 0;
    for c in text.chars() {
        if c == '\t' {
            let n = 4 - col % 4;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(c);
            col += 1;
        }
    }
    (out, col)
}

/// Mark the differing middle of a paired line, excluding the common
/// char-aligned prefix and suffix. An empty middle stays `None`.
fn trim(old: &mut Cell, new: &mut Cell) {
    let (a, b) = (old.text.as_str(), new.text.as_str());
    // (bytes, chars) of the common prefix and suffix.
    let common = |pairs: &mut dyn Iterator<Item = (char, char)>| {
        pairs
            .take_while(|(x, y)| x == y)
            .fold((0, 0), |(bytes, chars), (x, _)| {
                (bytes + x.len_utf8(), chars + 1)
            })
    };
    let (prefix, prefix_chars) = common(&mut a.chars().zip(b.chars()));
    let (suffix, suffix_chars) =
        common(&mut a[prefix..].chars().rev().zip(b[prefix..].chars().rev()));
    let span = |start: usize, end: usize| (start < end).then_some(start..end);
    old.hot = span(prefix, a.len() - suffix);
    new.hot = span(prefix, b.len() - suffix);
    old.hot_chars = span(prefix_chars, old.chars - suffix_chars);
    new.hot_chars = span(prefix_chars, new.chars - suffix_chars);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(bytes: &str) -> Doc {
        match parse(bytes.as_bytes()).unwrap() {
            Diff::Doc(d) => d,
            other => panic!("{other:?}"),
        }
    }
    fn kinds(d: &Doc) -> Vec<Kind> {
        d.rows.iter().map(|r| r.kind).collect()
    }
    const HEAD: &str = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n";

    #[test]
    fn pairs_removed_runs_with_following_added_runs_and_pads() {
        use Kind::*;
        let d = doc(&format!(
            "{HEAD}@@ -1,6 +1,9 @@\n a\n-b\n-c\n-d\n+B\n+C\n+D\n+E\n+F\n+G\n e\n f\n"
        ));
        // 3 removed then 6 added: 6 rows, 3 Modified + 3 Added.
        assert_eq!(
            kinds(&d),
            [
                Same, Modified, Modified, Modified, Added, Added, Added, Same, Same
            ]
        );
        assert_eq!(
            d.blocks,
            [Block {
                rows: 1..7,
                kind: Modified
            }]
        );
        assert!(d.rows[4].old.is_none());
        assert_eq!(d.rows[4].new.as_ref().unwrap().line, 5);
        assert_eq!(d.rows[7].old.as_ref().unwrap().line, 5);
        assert_eq!(d.rows[7].new.as_ref().unwrap().line, 8);
        assert_eq!(d.max_line, 9);

        let d = doc(&format!("{HEAD}@@ -1,4 +1,2 @@\n-a\n-b\n-c\n+A\n x\n"));
        assert_eq!(kinds(&d), [Modified, Removed, Removed, Same]);
        assert!(d.rows[1].new.is_none());
    }

    #[test]
    fn lone_runs_and_adjacent_blocks_are_separate_blocks() {
        use Kind::*;
        let d = doc(&format!(
            "{HEAD}@@ -1,4 +1,4 @@\n+new\n a\n-gone\n b\n-x\n+y\n"
        ));
        assert_eq!(kinds(&d), [Added, Same, Removed, Same, Modified]);
        assert_eq!(
            d.blocks,
            [
                Block {
                    rows: 0..1,
                    kind: Added
                },
                Block {
                    rows: 2..3,
                    kind: Removed
                },
                Block {
                    rows: 4..5,
                    kind: Modified
                },
            ]
        );
    }

    #[test]
    fn strict_single_hunk_rejects_partial_or_corrupt_output() {
        // Shape of git 2.39's -U<INT_MAX> overflow: repeated hunks from line 1.
        let twice = format!("{HEAD}@@ -1,1 +1,1 @@\n-a\n+b\n@@ -1,1 +1,2 @@\n-a\n+b\n+c\n");
        assert_eq!(
            parse(twice.as_bytes()).unwrap(),
            Diff::Notice(Notice::TooLarge)
        );
        let short = format!("{HEAD}@@ -1,3 +1,3 @@\n a\n");
        assert_eq!(
            parse(short.as_bytes()).unwrap(),
            Diff::Notice(Notice::TooLarge)
        );
        let late = format!("{HEAD}@@ -40,2 +40,2 @@\n-a\n+b\n c\n");
        assert_eq!(
            parse(late.as_bytes()).unwrap(),
            Diff::Notice(Notice::TooLarge)
        );
        assert!(parse(format!("{HEAD}@@ nonsense @@\n").as_bytes()).is_err());
        assert!(parse(format!("{HEAD}@@ -1 +1 @@\n?what\n").as_bytes()).is_err());
    }

    #[test]
    fn notices_for_binary_submodule_and_no_content_change() {
        let bin = "diff --git a/i.png b/i.png\nindex 1..2 100644\nBinary files a/i.png and b/i.png differ\n";
        assert_eq!(parse(bin.as_bytes()).unwrap(), Diff::Notice(Notice::Binary));
        let sub = "diff --git a/s b/s\nindex 1111111..2222222 160000\n--- a/s\n+++ b/s\n@@ -1 +1 @@\n-Subproject commit 1111111\n+Subproject commit 2222222\n";
        assert_eq!(
            parse(sub.as_bytes()).unwrap(),
            Diff::Notice(Notice::Submodule)
        );
        let new_sub = "diff --git a/s b/s\nnew file mode 160000\nindex 0000000..2222222\n--- /dev/null\n+++ b/s\n@@ -0,0 +1 @@\n+Subproject commit 2222222\n";
        assert_eq!(
            parse(new_sub.as_bytes()).unwrap(),
            Diff::Notice(Notice::Submodule)
        );
        let empty_add = "diff --git a/e b/e\nnew file mode 100644\nindex 0000000..e69de29\n";
        assert_eq!(
            parse(empty_add.as_bytes()).unwrap(),
            Diff::Notice(Notice::Unchanged)
        );
        assert_eq!(parse(b"").unwrap(), Diff::Notice(Notice::Unchanged));
    }

    #[test]
    fn added_and_deleted_files_start_at_zero() {
        use Kind::*;
        let d = doc(
            "diff --git a/n b/n\nnew file mode 100644\n--- /dev/null\n+++ b/n\n@@ -0,0 +1,2 @@\n+one\n+two\n",
        );
        assert_eq!(kinds(&d), [Added, Added]);
        assert_eq!(d.rows[1].new.as_ref().unwrap().line, 2);
        let d = doc(
            "diff --git a/n b/n\ndeleted file mode 100644\n--- a/n\n+++ /dev/null\n@@ -1 +0,0 @@\n-one\n",
        );
        assert_eq!(kinds(&d), [Removed]);
    }

    #[test]
    fn no_newline_marker_and_crlf() {
        use Kind::*;
        // Only the final newline changed: a Modified row with nothing hot.
        let d = doc(&format!(
            "{HEAD}@@ -1,2 +1,2 @@\n a\n-end\n\\ No newline at end of file\n+end\n"
        ));
        assert_eq!(kinds(&d), [Same, Modified]);
        let row = &d.rows[1];
        assert!(row.old.as_ref().unwrap().no_eol);
        assert!(!row.new.as_ref().unwrap().no_eol);
        assert_eq!(row.old.as_ref().unwrap().hot, None);
        assert_eq!(row.new.as_ref().unwrap().hot, None);
        // CRLF is displayed as LF; a pure line-ending change still shows as Modified.
        let d = doc(&format!("{HEAD}@@ -1 +1 @@\n-same\r\n+same\n"));
        assert_eq!(kinds(&d), [Modified]);
        assert_eq!(d.rows[0].old.as_ref().unwrap().text, "same");
        // An empty context line (diff.suppressBlankEmpty) is still a context line.
        let d = doc(&format!("{HEAD}@@ -1,3 +1,3 @@\n a\n\n-b\n+c\n"));
        assert_eq!(kinds(&d), [Same, Same, Modified]);
    }

    #[test]
    fn tabs_expand_to_four_column_stops() {
        let d = doc(&format!("{HEAD}@@ -1 +1 @@\n-a\tb\n+\tab\tc\n"));
        assert_eq!(d.rows[0].old.as_ref().unwrap().text, "a   b");
        assert_eq!(d.rows[0].new.as_ref().unwrap().text, "    ab  c");
        assert_eq!(d.max_cols, 9);
    }

    #[test]
    fn trim_spans_multibyte_chars() {
        let d = doc(&format!(
            "{HEAD}@@ -1 +1 @@\n-let café = 1;\n+let caféé = 12;\n"
        ));
        let (old, new) = (
            d.rows[0].old.as_ref().unwrap(),
            d.rows[0].new.as_ref().unwrap(),
        );
        fn hot(c: &Cell) -> &str {
            &c.text[c.hot.clone().unwrap()]
        }
        assert_eq!(hot(old), " = 1");
        assert_eq!(hot(new), "é = 12");
        // Char offsets for painting: "let café" is 8 chars (9 bytes).
        assert_eq!((old.chars, new.chars), (13, 15));
        assert_eq!(old.hot, Some(9..13));
        assert_eq!(old.hot_chars, Some(8..12));
        assert_eq!(new.hot, Some(9..16));
        assert_eq!(new.hot_chars, Some(8..14));
        for c in [old, new] {
            assert_eq!(c.chars, c.text.chars().count());
            let (h, hc) = (c.hot.clone().unwrap(), c.hot_chars.clone().unwrap());
            assert_eq!(c.text[..h.start].chars().count(), hc.start);
            assert_eq!(c.text[..h.end].chars().count(), hc.end);
        }
        // Pure insertion inside a line: only the new side is hot.
        let d = doc(&format!("{HEAD}@@ -1 +1 @@\n-ab\n+aXb\n"));
        assert_eq!(d.rows[0].old.as_ref().unwrap().hot, None);
        assert_eq!(d.rows[0].new.as_ref().unwrap().hot, Some(1..2));
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        // Deterministic sweep over diff-shaped garbage: headers, markers,
        // CR, tabs, a multibyte char, and invalid UTF-8.
        let alphabet: &[&[u8]] = &[
            b"@@ -1,2 +1,3 @@\n",
            b"@@ -0,0 +1 @@\n",
            b"\n",
            b" ",
            b"-",
            b"+",
            b"\\ No newline at end of file\n",
            b"\r",
            b"\t",
            "é".as_bytes(),
            b"\xff\xfe",
            b"a",
            b"Binary files ",
            b"index 1..2 160000\n",
        ];
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..5000 {
            let mut input = Vec::new();
            for _ in 0..(seed % 24) {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                input.extend_from_slice(alphabet[(seed % alphabet.len() as u64) as usize]);
            }
            let _ = parse(&input);
            // Every prefix too, so truncation at any byte is covered.
            for cut in 0..input.len() {
                let _ = parse(&input[..cut]);
            }
        }
    }
}
