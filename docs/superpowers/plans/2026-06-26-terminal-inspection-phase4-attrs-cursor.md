# Terminal Inspection Phase 4: `--attrs` and `--cursor` Opt-ins

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `--attrs` (per-cell color+style data) and `--cursor` (cursor position/shape) as opt-in flags to `foreman snapshot`, wired end-to-end from `inspect.rs` through `control.rs` to `wm.rs`, with the wire staying byte-compat when neither flag is passed.

**Architecture:** `CellData` and `snapshot_cells` live in `inspect.rs` alongside the existing `snapshot_text`/`cursor_info`. `Session` gains two thin delegation methods. `OpenReply` gains two new optional fields (wire-compat via `skip_serializing_if`); `Default` is derived so the many existing literal sites don't all need updating. `SnapshotRequest` gains `attrs: bool` and `cursor: bool`. `snapshot_dispatch` in `wm.rs` is updated to call the new inspect fns when flags are set.

**Tech Stack:** Rust, alacritty_terminal, serde_json, egui::Color32.

## Global Constraints

- Windows, GNU toolchain (not MSVC). Build: `$env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"; cargo build`.
- Run tests with `cargo test` — must finish 100% green before done.
- Do NOT implement `--region`, `--rows`, `--wait-for`, `--since-seq` — those are later phases.
- Default `snapshot` (no flags) must return ONLY `history` — `cells` and `cursor` stay `None`, omitted on the wire.
- Never change DSR/ready/Listener/read_input/pump's latch logic.
- Keep all 249 existing tests green.
- `VoidListener` is fine in `inspect.rs` tests (no live PTY = no DSR trap).

## File Map

- **Modify:** `src/inspect.rs` — add `CellData` struct + `snapshot_cells` fn + tests.
- **Modify:** `src/terminal.rs` — `resolve` → `pub(crate)`, add `snapshot_cells` and `cursor_info` delegation methods to `Session`.
- **Modify:** `src/control.rs` — `SnapshotRequest`: add `attrs`/`cursor` bool fields; `OpenReply`: derive `Default`, add `cells`/`cursor` optional fields, update `err()` and all literal sites; `parse_snapshot_args`: handle `--attrs`/`--cursor`; `report()`: print JSON when `cells` or `cursor` is set; update `HELP_SNAPSHOT`; add tests.
- **Modify:** `src/wm.rs` — `snapshot_dispatch`: return structured tuple; `CtrlMsg::Snapshot` arm: build full `OpenReply` from tuple.

---

### Task 1: `CellData` struct + `snapshot_cells` fn in `inspect.rs` (TDD)

**Files:**
- Modify: `src/inspect.rs`

**Interfaces:**
- Consumes: `crate::terminal::resolve` (to be made `pub(crate)` in Task 2 — but for Task 1 tests, call `resolve` directly inside `inspect.rs` by inlining a local version OR just add a `pub(crate)` stub in `terminal.rs` first).
- Produces: `pub struct CellData { ... }`, `pub fn snapshot_cells<L: EventListener>(term: &Term<L>, region: Option<Region>) -> Vec<Vec<CellData>>`

**Implementation note on `resolve`:** `resolve` is currently a private fn in `terminal.rs`. Task 2 makes it `pub(crate)`. For Task 1, do Task 2's `pub(crate)` change FIRST (it's a one-word change), then implement `snapshot_cells` in `inspect.rs` calling `crate::terminal::resolve`.

- [ ] **Step 1: Make `resolve` pub(crate) in `terminal.rs`**

In `src/terminal.rs`, line 57, change:
```rust
fn resolve(c: AnsiColor) -> Option<egui::Color32> {
```
to:
```rust
pub(crate) fn resolve(c: AnsiColor) -> Option<egui::Color32> {
```

- [ ] **Step 2: Add `CellData` struct to `src/inspect.rs`**

Add after the `CursorInfo` struct (around line 37):

```rust
/// Per-cell rendering data for the `--attrs` opt-in.
#[derive(Debug, Clone, serde::Serialize)]
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
```

- [ ] **Step 3: Write the failing `snapshot_cells` tests in `src/inspect.rs`**

Add inside `mod tests { ... }`, after the existing tests:

```rust
// ---- snapshot_cells ----------------------------------------------------------
#[test]
fn snapshot_cells_plain_cell_all_flags_false() {
    let term = term_with(b"A", 20, 1);
    let grid = snapshot_cells(&term, None);
    assert_eq!(grid.len(), 1);
    let row = &grid[0];
    // first cell should be 'A'
    assert_eq!(row[0].ch, 'A');
    assert!(!row[0].bold);
    assert!(!row[0].italic);
    assert!(!row[0].underline);
    assert!(!row[0].strikethrough);
    assert!(!row[0].inverse);
    assert!(!row[0].dim);
    assert!(!row[0].wide);
}

#[test]
fn snapshot_cells_underline_flag() {
    // ESC[4m = underline on; ESC[0m = reset
    let term = term_with(b"\x1b[4mU\x1b[0m", 20, 1);
    let grid = snapshot_cells(&term, None);
    // find the 'U' cell
    let u_cell = grid[0].iter().find(|c| c.ch == 'U').expect("U not found");
    assert!(u_cell.underline, "expected underline=true for ESC[4m cell");
}

#[test]
fn snapshot_cells_inverse_flag() {
    // ESC[7m = inverse on
    let term = term_with(b"\x1b[7mI\x1b[0m", 20, 1);
    let grid = snapshot_cells(&term, None);
    let i_cell = grid[0].iter().find(|c| c.ch == 'I').expect("I not found");
    assert!(i_cell.inverse, "expected inverse=true for ESC[7m cell");
}

#[test]
fn snapshot_cells_empty_region_clamps_without_panic() {
    let term = term_with(b"hello", 20, 3);
    // A region larger than the grid must clamp, not panic
    let grid = snapshot_cells(&term, Some(Region { row: 0, col: 0, rows: 99, cols: 99 }));
    assert_eq!(grid.len(), 3);
}

#[test]
fn snapshot_cells_skips_wide_char_spacer() {
    // CJK wide char: one WIDE_CHAR cell + one WIDE_CHAR_SPACER
    // The output row should have the wide char cell, not the spacer
    let term = term_with("漢".as_bytes(), 20, 1);
    let grid = snapshot_cells(&term, None);
    // The row should contain 漢 with wide=true, but NOT a spacer cell
    let han = grid[0].iter().find(|c| c.ch == '漢');
    assert!(han.is_some(), "expected 漢 in output");
    assert!(han.unwrap().wide, "expected wide=true for CJK cell");
    // Spacer cells (ch='\0' from the spacer) should not appear
    assert!(!grid[0].iter().any(|c| c.ch == ' ' && grid[0].iter().filter(|x| x.ch != ' ').count() > 0 && c.wide),
        "spacer should be skipped");
}
```

- [ ] **Step 4: Run tests to verify they FAIL (function not yet defined)**

```powershell
$env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
cargo test snapshot_cells 2>&1 | Select-Object -Last 20
```

Expected: compile error "cannot find function `snapshot_cells`".

- [ ] **Step 5: Implement `snapshot_cells` in `src/inspect.rs`**

Add after `grid_contains` (around line 97):

```rust
/// Per-cell attribute snapshot for the `--attrs` opt-in. Same region logic and
/// wide-char handling as `snapshot_text`; returns one `CellData` per kept cell.
pub fn snapshot_cells<L: EventListener>(term: &Term<L>, region: Option<Region>) -> Vec<Vec<CellData>> {
    use alacritty_terminal::vte::ansi::Color as AnsiColor;
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
            // Skip wide-char spacer cells (same rule as snapshot_text)
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                col += 1;
                continue;
            }
            let fg_color = crate::terminal::resolve(cell.fg).unwrap_or(crate::terminal::FG);
            let bg_color = crate::terminal::resolve(cell.bg).map(|c| [c.r(), c.g(), c.b()]);
            row_cells.push(CellData {
                ch: if cell.c == '\0' { ' ' } else { cell.c },
                fg: [fg_color.r(), fg_color.g(), fg_color.b()],
                bg: bg_color,
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
```

**Important:** The `alacritty_terminal::vte::ansi::Color as AnsiColor` import inside the fn is needed only if not already in scope at module level. Since `inspect.rs` uses `alacritty_terminal::term::cell::Flags` at the top already, and `AnsiColor` is used implicitly via `cell.fg`/`cell.bg` (which are `AnsiColor` values), the import is already covered by `crate::terminal::resolve` accepting an `AnsiColor`. No extra import needed in the fn body — remove the `use` line if rustc complains.

- [ ] **Step 6: Run the new tests**

```powershell
$env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
cargo test snapshot_cells 2>&1 | Select-Object -Last 30
```

Expected: all `snapshot_cells_*` tests PASS. If there are compile errors about `AnsiColor` not in scope inside the fn, remove the `use` line — the type flows through the cell reference automatically.

- [ ] **Step 7: Run the full suite to confirm no regressions**

```powershell
cargo test 2>&1 | Select-Object -Last 20
```

Expected: all tests pass (249 + 5 new = 254 total).

---

### Task 2: `Session` delegation methods in `terminal.rs`

**Files:**
- Modify: `src/terminal.rs`

**Interfaces:**
- Consumes: `crate::inspect::CellData`, `crate::inspect::CursorInfo`, `crate::inspect::Region`, `crate::inspect::snapshot_cells`, `crate::inspect::cursor_info`
- Produces:
  - `pub fn snapshot_cells(&mut self, region: Option<crate::inspect::Region>) -> Vec<Vec<crate::inspect::CellData>>`
  - `pub fn cursor_info(&mut self) -> crate::inspect::CursorInfo`

(Note: `resolve` was already made `pub(crate)` in Task 1 Step 1. This task only adds the two `Session` methods.)

- [ ] **Step 1: Add `snapshot_cells` and `cursor_info` methods to `Session` in `terminal.rs`**

After the existing `pub fn snapshot_text` method (around line 516), add:

```rust
/// Pump pending PTY output, then return per-cell attribute data for `--attrs`.
pub fn snapshot_cells(
    &mut self,
    region: Option<crate::inspect::Region>,
) -> Vec<Vec<crate::inspect::CellData>> {
    self.pump();
    crate::inspect::snapshot_cells(&self.term, region)
}

/// Pump pending PTY output, then return cursor position + shape for `--cursor`.
pub fn cursor_info(&mut self) -> crate::inspect::CursorInfo {
    self.pump();
    crate::inspect::cursor_info(&self.term)
}
```

- [ ] **Step 2: Build to verify no compile errors**

```powershell
$env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
cargo build 2>&1 | Select-Object -Last 20
```

Expected: `Finished` with no errors.

- [ ] **Step 3: Run full suite**

```powershell
cargo test 2>&1 | Select-Object -Last 20
```

Expected: all tests pass.

---

### Task 3: `OpenReply` — derive `Default`, add `cells`/`cursor` fields, convert all literal sites

**Files:**
- Modify: `src/control.rs` — `OpenReply` struct + `err()` impl + all literal sites in `control.rs`
- Modify: `src/wm.rs` — all `OpenReply { ... }` literal sites

This is the most mechanical task. The strategy: derive `Default` on `OpenReply`, then update EVERY `OpenReply { ... }` literal to use `..Default::default()` so adding fields doesn't break future additions. The build error list will guide us.

**Interfaces:**
- Produces: `OpenReply` with new fields `cells: Option<Vec<Vec<crate::inspect::CellData>>>` and `cursor: Option<crate::inspect::CursorInfo>`, plus `#[derive(Default)]`.

- [ ] **Step 1: Update `OpenReply` struct definition in `src/control.rs`**

Find the `OpenReply` struct (lines 34–52). Replace it entirely:

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
pub struct OpenReply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Generic line-per-line payload — chat `--history` results and `status`
    /// listings both ride here; `report()` prints it line per line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<String>>,
    /// The posted message's seq — what a later reply cites via `--re`. Set
    /// only on a successful post reply; skipped on the wire when None so v1
    /// replies stay byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// Per-cell attribute data for `--attrs` opt-in. None (omitted on wire)
    /// when `--attrs` was not requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cells: Option<Vec<Vec<crate::inspect::CellData>>>,
    /// Cursor position and shape for `--cursor` opt-in. None (omitted on wire)
    /// when `--cursor` was not requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<crate::inspect::CursorInfo>,
}
```

**Note:** `Default` is derived. `ok: bool` defaults to `false` which is correct for the zero value. `OpenReply::err()` sets `ok: false` explicitly so this is fine.

- [ ] **Step 2: Update `OpenReply::err()` to use `..Default::default()`**

Replace the existing `err()` body:

```rust
impl OpenReply {
    pub fn err(msg: impl Into<String>) -> Self {
        OpenReply {
            ok: false,
            error: Some(msg.into()),
            ..Default::default()
        }
    }
}
```

- [ ] **Step 3: Attempt build to see ALL broken literal sites**

```powershell
$env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
cargo build 2>&1 | Select-Object -Last 60
```

This will list every `OpenReply { ... }` literal that is missing the new fields. Copy the output — it tells you exactly which lines to fix in `control.rs` and `wm.rs`.

- [ ] **Step 4: Fix all `OpenReply { ... }` literals in `src/control.rs`**

For EVERY literal that still names all fields explicitly (the compiler will list them), add `..Default::default()` and remove the `cells: None, cursor: None` lines (they don't exist yet, and `Default` handles them). The pattern is:

Before:
```rust
OpenReply {
    ok: true,
    terminal: None,
    project: None,
    error: None,
    history: Some(lines),
    seq: None,
}
```

After:
```rust
OpenReply {
    ok: true,
    history: Some(lines),
    ..Default::default()
}
```

Omit any field that is `None` — `Default` provides `None` for all `Option` fields. Keep only fields that are non-default (i.e. `ok: true`, and any `Some(...)` fields).

The full set of sites in `control.rs` (line numbers approximate — confirm from cargo output):
- `OpenReply::err()` — already done in Step 2.
- Test literal at ~line 905 (the `reply_roundtrips_and_omits_none_fields` test) — update to use `..Default::default()`.
- Test literal at ~line 1023 (send pipe roundtrip) — update.
- Test literal at ~line 1095 — update.
- Test literal at ~line 1246 — update.
- `ok_reply` at ~line 1294 — update.
- History reply at ~line 1304 — update.
- Test literal at ~line 1426 — update.
- Literals at ~lines 1560, 1569 — update.
- Literals at ~lines 1828, 1871 — update.

- [ ] **Step 5: Fix all `OpenReply { ... }` literals in `src/wm.rs`**

Same pattern. Sites in `wm.rs` (approximate lines — use cargo build output to confirm):
- ~line 858
- ~line 879
- ~line 899 — this is the snapshot reply; see Task 4 for its final form
- ~line 919
- ~line 955
- ~line 992 — this is the current snapshot arm; see Task 4 for its final form
- `open_reply` fn at ~line 1009
- `ok_reply` closure at ~line 1299

For now, convert them ALL to `..Default::default()` style. The snapshot arm (`992`) is fine with `history: Some(lines)` for now; Task 4 will replace it entirely.

- [ ] **Step 6: Build clean**

```powershell
$env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
cargo build 2>&1 | Select-Object -Last 20
```

Expected: `Finished` with zero errors.

- [ ] **Step 7: Run full suite**

```powershell
cargo test 2>&1 | Select-Object -Last 20
```

Expected: all tests pass. The existing `reply_roundtrips_and_omits_none_fields` test should still pass — it checks that `cells` and `cursor` keys are absent when not set (which `skip_serializing_if = "Option::is_none"` guarantees automatically).

---

### Task 4: `SnapshotRequest` flags + `parse_snapshot_args` + `report()` + wm dispatch

**Files:**
- Modify: `src/control.rs` — `SnapshotRequest`, `parse_snapshot_args`, `report`, `HELP_SNAPSHOT`, new tests
- Modify: `src/wm.rs` — `snapshot_dispatch` return type + `CtrlMsg::Snapshot` arm

**Interfaces:**
- Consumes: `Session::snapshot_cells`, `Session::cursor_info` (from Task 2); `CellData`, `CursorInfo` (from Task 1)
- Produces: `foreman snapshot --attrs` populates `reply.cells`; `foreman snapshot --cursor` populates `reply.cursor`; default snapshot unchanged.

- [ ] **Step 1: Add `attrs` and `cursor` bool fields to `SnapshotRequest` in `src/control.rs`**

Find:
```rust
/// Read the rendered viewport of a terminal as plain text rows.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SnapshotRequest {
    pub cmd: String, // always "snapshot"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
}
```

Replace with:

```rust
fn is_false(b: &bool) -> bool {
    !*b
}

/// Read the rendered viewport of a terminal as plain text rows.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SnapshotRequest {
    pub cmd: String, // always "snapshot"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<String>,
    /// `--attrs`: if true, reply includes `cells` (per-cell colors + flags).
    #[serde(default, skip_serializing_if = "is_false")]
    pub attrs: bool,
    /// `--cursor`: if true, reply includes `cursor` (position + shape).
    #[serde(default, skip_serializing_if = "is_false")]
    pub cursor: bool,
}
```

- [ ] **Step 2: Write the failing `parse_snapshot_args` flag tests**

Add to `mod tests` in `src/control.rs`:

```rust
#[test]
fn parse_snapshot_args_attrs_flag() {
    let req = parse_snapshot_args(
        &s(&["--project", "p1", "--terminal", "t2", "--attrs"]),
        None,
        None,
    )
    .unwrap();
    assert!(req.attrs, "expected attrs=true");
    assert!(!req.cursor, "expected cursor=false by default");
}

#[test]
fn parse_snapshot_args_cursor_flag() {
    let req = parse_snapshot_args(
        &s(&["--project", "p1", "--terminal", "t2", "--cursor"]),
        None,
        None,
    )
    .unwrap();
    assert!(req.cursor, "expected cursor=true");
    assert!(!req.attrs, "expected attrs=false by default");
}

#[test]
fn snapshot_reply_without_attrs_cursor_omits_those_keys() {
    // Wire-compat: a plain snapshot reply must not include "cells" or "cursor" keys.
    let reply = OpenReply {
        ok: true,
        history: Some(vec!["row0".into()]),
        ..Default::default()
    };
    let json = serde_json::to_string(&reply).unwrap();
    assert!(!json.contains("\"cells\""), "cells must be absent: {json}");
    assert!(!json.contains("\"cursor\""), "cursor must be absent: {json}");
}
```

- [ ] **Step 3: Run tests to verify they FAIL**

```powershell
$env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
cargo test parse_snapshot_args_attrs_flag 2>&1 | Select-Object -Last 15
```

Expected: compile error — `parse_snapshot_args` doesn't handle `--attrs` yet.

- [ ] **Step 4: Update `parse_snapshot_args` to handle `--attrs` and `--cursor`**

Find the `parse_snapshot_args` function body (around line 575). Add two new match arms inside the `while i < args.len()` loop, before the `other if other.starts_with("--")` catch-all:

```rust
"--attrs" => {
    attrs = true;
    i += 1;
}
"--cursor" => {
    cursor = true;
    i += 1;
}
```

Also add `let mut attrs = false; let mut cursor = false;` at the top of the function body (after the existing `let mut terminal: Option<String> = None;` line), and include them in the returned `SnapshotRequest`:

```rust
Ok(SnapshotRequest {
    cmd: "snapshot".into(),
    project,
    terminal: Some(terminal),
    attrs,
    cursor,
})
```

- [ ] **Step 5: Update `HELP_SNAPSHOT` in `src/control.rs`**

Find:
```rust
const HELP_SNAPSHOT: &str = "\
foreman snapshot [--project P] [--terminal T]

Read terminal T's rendered viewport as plain text (default: your own).
One string per visible row, trailing spaces trimmed, printed line per line.
Reply rides the same history field as status. A snapshot of a settled
terminal (after foreman send) gives you the current screen state.
Exit codes: 0 ok, 1 refused/unreachable, 2 bad arguments.";
```

Replace with:
```rust
const HELP_SNAPSHOT: &str = "\
foreman snapshot [--project P] [--terminal T] [--attrs] [--cursor]

Read terminal T's rendered viewport (default: your own).
Default: plain text rows in the history field, one per visible row, trailing
spaces trimmed. Opt-in additions (new JSON fields on the reply):
  --attrs   cells: per-cell fg/bg (RGB), bold/italic/underline/strikethrough/
            inverse/dim/wide flags. Use --region to bound the JSON size.
  --cursor  cursor: {row, col, shape} from the emulator's renderable content.
When --attrs or --cursor is set the whole reply is printed as JSON.
Exit codes: 0 ok, 1 refused/unreachable, 2 bad arguments.";
```

- [ ] **Step 6: Update `report()` to print JSON when `cells` or `cursor` is set**

Find the `report` function:
```rust
fn report(label: &str, res: std::io::Result<OpenReply>) -> i32 {
    match res {
        Ok(r) if r.ok => {
            if let Some(lines) = &r.history {
                for l in lines {
                    println!("{l}");
                }
            } else {
                println!("{}", serde_json::to_string(&r).unwrap_or_default());
            }
            0
        }
```

Replace the `Ok(r) if r.ok` arm with:
```rust
        Ok(r) if r.ok => {
            if r.cells.is_some() || r.cursor.is_some() {
                // Structured reply: print the whole JSON so the caller gets all fields.
                println!("{}", serde_json::to_string(&r).unwrap_or_default());
            } else if let Some(lines) = &r.history {
                for l in lines {
                    println!("{l}");
                }
            } else {
                println!("{}", serde_json::to_string(&r).unwrap_or_default());
            }
            0
        }
```

- [ ] **Step 7: Update `snapshot_dispatch` in `src/wm.rs`**

Find:
```rust
fn snapshot_dispatch(
    &mut self,
    req: &crate::control::SnapshotRequest,
) -> Result<Vec<String>, String> {
    let terminal = req.terminal.as_deref().ok_or("snapshot: missing terminal")?;
    let (pid, tid) = self.resolve_terminal(req.project.as_deref(), terminal)?;
    let session = self.session_mut(pid, tid)?;
    Ok(session.snapshot_text(None))
}
```

Replace with:
```rust
fn snapshot_dispatch(
    &mut self,
    req: &crate::control::SnapshotRequest,
) -> Result<
    (
        Vec<String>,
        Option<Vec<Vec<crate::inspect::CellData>>>,
        Option<crate::inspect::CursorInfo>,
    ),
    String,
> {
    let terminal = req.terminal.as_deref().ok_or("snapshot: missing terminal")?;
    let (pid, tid) = self.resolve_terminal(req.project.as_deref(), terminal)?;
    let session = self.session_mut(pid, tid)?;
    let lines = session.snapshot_text(None);
    let cells = if req.attrs { Some(session.snapshot_cells(None)) } else { None };
    let cursor = if req.cursor { Some(session.cursor_info()) } else { None };
    Ok((lines, cells, cursor))
}
```

- [ ] **Step 8: Update the `CtrlMsg::Snapshot` arm in `src/wm.rs`**

Find the `CtrlMsg::Snapshot` arm (around line 987):
```rust
CtrlMsg::Snapshot(req, reply, sent) => {
    if sent.elapsed() >= REPLY_TIMEOUT {
        return;
    }
    let _ = reply.send(match self.snapshot_dispatch(&req) {
        Ok(lines) => OpenReply {
            ok: true,
            terminal: None,
            project: None,
            error: None,
            history: Some(lines),
            seq: None,
        },
        Err(e) => OpenReply::err(e),
    });
}
```

Replace with:
```rust
CtrlMsg::Snapshot(req, reply, sent) => {
    if sent.elapsed() >= REPLY_TIMEOUT {
        return;
    }
    let _ = reply.send(match self.snapshot_dispatch(&req) {
        Ok((lines, cells, cursor)) => OpenReply {
            ok: true,
            history: Some(lines),
            cells,
            cursor,
            ..Default::default()
        },
        Err(e) => OpenReply::err(e),
    });
}
```

- [ ] **Step 9: Build clean**

```powershell
$env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue
cargo build 2>&1 | Select-Object -Last 30
```

Expected: `Finished` with zero errors. If the existing `snapshot_pipe_roundtrip` test still uses the old `SnapshotRequest` literal (missing `attrs`/`cursor`), it will still work because those fields have `#[serde(default)]` — but the struct literal in the test code will need updating since Rust struct literals require all fields or `..Default::default()`. Update the test:

```rust
let req = SnapshotRequest {
    cmd: "snapshot".into(),
    project: Some("p1".into()),
    terminal: Some("t3".into()),
    attrs: false,
    cursor: false,
};
```

Also update the `OpenReply` literal inside the snapshot pipe roundtrip test to use `..Default::default()`.

- [ ] **Step 10: Run all tests**

```powershell
cargo test 2>&1 | Select-Object -Last 30
```

Expected: all tests pass (target ~260+). Specifically check:
- `snapshot_cells_*` tests — green
- `parse_snapshot_args_attrs_flag` — green
- `parse_snapshot_args_cursor_flag` — green
- `snapshot_reply_without_attrs_cursor_omits_those_keys` — green
- `snapshot_pipe_roundtrip` — green
- `reply_roundtrips_and_omits_none_fields` — green (the existing wire-compat test)

---

## Spec Coverage Self-Check

| Requirement | Task |
|---|---|
| `CellData` struct with all fields | Task 1 |
| `snapshot_cells` fn in inspect.rs | Task 1 |
| TDD tests: underline, inverse, plain, clamp, wide-spacer | Task 1 |
| `resolve` → `pub(crate)` | Task 1 Step 1 |
| `Session::snapshot_cells` delegation | Task 2 |
| `Session::cursor_info` delegation | Task 2 |
| `OpenReply` gets `cells`/`cursor` optional fields | Task 3 |
| `OpenReply` derives `Default` | Task 3 |
| All `OpenReply { ... }` literals converted to `..Default::default()` style | Task 3 |
| `is_false` helper for `skip_serializing_if` | Task 4 Step 1 |
| `SnapshotRequest.attrs` / `.cursor` bool fields | Task 4 Step 1 |
| `parse_snapshot_args` handles `--attrs` / `--cursor` | Task 4 |
| Tests: `parse_snapshot_args_attrs_flag`, `parse_snapshot_args_cursor_flag` | Task 4 |
| Wire-compat test: plain reply omits `cells`/`cursor` keys | Task 4 |
| `HELP_SNAPSHOT` updated | Task 4 |
| `report()` prints JSON when `cells`/`cursor` set | Task 4 |
| `snapshot_dispatch` returns structured tuple | Task 4 |
| `CtrlMsg::Snapshot` arm builds full `OpenReply` | Task 4 |
| Default snapshot (no flags) still returns only `history` | enforced by `req.attrs`/`req.cursor` defaulting false |
| `--region`, `--rows`, `--wait-for`, `--since-seq` NOT implemented | (out of scope, confirmed) |
| All 249+ existing tests stay green | verified in each task's test step |
