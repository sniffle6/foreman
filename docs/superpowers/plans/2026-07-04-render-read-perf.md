# Render & Read-Path Performance Cleanup — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut foreman's two biggest hot-path costs — the 8 KiB PTY read chunking (parse-side) and the redundant per-frame grid galley rebuild for unchanged panes (draw-side) — without changing any visible behavior or breaking a hot-path invariant.

**Architecture:** Two independent fixes. **#1** widens the PTY reader buffer 8 KiB → 64 KiB (a tuning constant; no interface change). **#2** adds a `Session`-level galley memo: the per-cell text walk (`frame::plan`) is split into a *cached* content half (`frame::text_rows`) and a *per-frame* overlay half (`frame::overlays`); `Session::show` rebuilds the `LayoutJob`/galley only when a cheap key `(content_gen, display_offset, cols, rows, font_px)` changes, otherwise re-clones the cached `Arc<Galley>`. A new `content_gen` counter — distinct from `output_gen` — is the single "grid content changed" signal so unchanged panes cost ~one Arc clone.

**Tech Stack:** Rust, egui 0.34, alacritty_terminal, portable-pty/ConPTY. Windows + GNU toolchain.

## Design (codebase-design seams)

- **#1 is not a module** — it's a tuning constant on the reader thread. `read()` returns whatever bytes are already available (it never waits to fill), so a bigger buffer only changes *chunk count under flood*, never latency or byte ordering. No seam, no interface. Just the value + a measurement.
- **#2's seam is the split of the existing pure `frame::plan`** into two pure functions at the same seam:
  - `frame::text_rows(grid, metrics) -> Vec<Vec<StyleRun>>` — the expensive content-only half. Cacheable: depends *only* on grid content + geometry.
  - `frame::overlays(grid, metrics, selection, cursor) -> Overlays` — the cheap per-frame half (selection highlights O(selected rows), caret O(1), thumb O(1)). Never cached: the caret settles over time and selection changes on drag.
  - The galley memo is a **thin** `Session` field, not a general cache module — there is exactly one cache and one caller, so a `Option<(GalleyKey, Arc<Galley>)>` field + inline key check is the right depth (a generic `Cache<K,V>` here would fail the deletion test).
- **`content_gen` vs `output_gen` — keep two honest signals.** `output_gen` means "the child produced PTY bytes" and is polled by the settle machinery for quiescence. The galley must also invalidate on the *one* grid mutation that is **not** child output — the `inject_note` banner flushed in `resize()`. Overloading `output_gen` there would make the settle machinery see phantom child activity. So a separate `content_gen` bumps on **every** grid-content mutation (pump + note flush); `output_gen`'s meaning is untouched.

## Global Constraints

- **Toolchain:** GNU (`rustup default stable-gnu`), linker w64devkit. Never MSVC.
- **Kill the app before building:** `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500` — else the link fails with `Access is denied (os error 5)`.
- **No new dependencies.** (Both fixes use only std + egui already in-tree.)
- **Never use `VoidListener`** in any Session test — shells hang on the startup DSR without the real `Listener`.
- **`cargo test` must stay green** and the expected-warning baseline must not grow (no new dead-code warnings).
- **Commit only the fix.** No `[DEBUG-perf]` instrumentation in any commit (it lives only in the orchestrator's throwaway measurement builds). End commit messages with:
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`
- Build/test loop: `cargo build 2>&1 | Select-Object -Last 20`, `cargo test` (bin crate — there is **no** `--lib` target).

---

## Phase 0 — Baseline metrics (orchestrator-run, BEFORE any code change)

Run on the current working tree (the 1.24 ConPTY change is present; it does not affect steady-state render/flood, so the Phase 0 → Phase 3 delta is purely the perf fixes). **Do not commit the instrumentation.** Record the numbers in the table at the bottom.

### 0.1 Add the throwaway frame harness

In `src/main.rs`, `App::ui`, replace the single line at `src/main.rs:372`:

```rust
        self.desktop.show(ui, area, true, egui::Id::new("desktop"));
```

with:

```rust
        // [DEBUG-perf] TEMP — REMOVE before commit. Per-frame desktop render cost.
        let __t0 = std::time::Instant::now();
        self.desktop.show(ui, area, true, egui::Id::new("desktop"));
        eprintln!("show={:.3}", __t0.elapsed().as_secs_f64() * 1e3);
```

Build the release exe:

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build --release 2>&1 | Select-Object -Last 5
```

### 0.2 Scenario A — idle draw floor (1 pane)

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
Start-Job -Name fm { cmd /c "H:\claude code\foreman\target\release\foreman.exe 2> H:\claude code\foreman\perf-A.log" } | Out-Null
Start-Sleep -Seconds 12   # let it sit idle
Stop-Process -Name foreman -Force
```

Reduce:

```powershell
$v = Select-String perf-A.log -Pattern 'show=([\d.]+)' | ForEach-Object { [double]$_.Matches[0].Groups[1].Value }
"A idle  n=$($v.Count) avg=$([math]::Round(($v|Measure-Object -Average).Average,3)) max=$([math]::Round(($v|Measure-Object -Maximum).Maximum,3))"
```

### 0.3 Scenario B — 12 panes, 1 flooding, 11 static (THE metric for #2)

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
Start-Job -Name fm { cmd /c "H:\claude code\foreman\target\release\foreman.exe 2> H:\claude code\foreman\perf-B.log" } | Out-Null
Start-Sleep -Seconds 3
$exe = ".\target\release\foreman.exe"
1..11 | ForEach-Object { & $exe open --project p1 -- powershell.exe | Out-Null }   # now 12 panes
Start-Sleep -Seconds 2
# flood exactly one pane, fire-and-forget (never settles)
& $exe send --project p1 --terminal t2 --settle-ms 0 --text "while(`$true){ 'x'*120 }" --keys Enter | Out-Null
Start-Sleep -Seconds 12
Stop-Process -Name foreman -Force
```

Reduce (report avg + max + p95):

```powershell
$v = Select-String perf-B.log -Pattern 'show=([\d.]+)' | ForEach-Object { [double]$_.Matches[0].Groups[1].Value }
$s = $v | Sort-Object
"B 1-flood/11-static  n=$($v.Count) avg=$([math]::Round(($v|Measure-Object -Average).Average,3)) p95=$([math]::Round($s[[int]($s.Count*0.95)],3)) max=$([math]::Round(($v|Measure-Object -Maximum).Maximum,3))"
```

### 0.4 Scenario C — single-pane flood drain time (THE metric for #1)

Generate a fixed 200k-line file once, then time `type` of it in one fresh pane (send→prompt-return), median of 3.

```powershell
if (-not (Test-Path bigflood.txt)) { 1..200000 | ForEach-Object { "line $_ the quick brown fox jumps over the lazy dog" } | Set-Content bigflood.txt }
$exe = ".\target\release\foreman.exe"
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
Start-Job -Name fm { cmd /c "H:\claude code\foreman\target\release\foreman.exe 2> H:\claude code\foreman\perf-C.log" } | Out-Null
Start-Sleep -Seconds 3
$times = foreach ($i in 1..3) {
  $o = & $exe open --project p1 -- powershell.exe | Out-String
  $tid = ([regex]'t\d+').Match($o).Value
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  & $exe send --project p1 --terminal $tid --settle-ms 0 --text "cmd /c type bigflood.txt" --keys Enter | Out-Null
  while ($sw.ElapsedMilliseconds -lt 30000) { if ((& $exe snapshot --project p1 --terminal $tid | Out-String) -match '(?m)^PS .*>\s*$') { break } }
  $sw.ElapsedMilliseconds
}
Stop-Process -Name foreman -Force
"C flood-drain ms (3 runs): $($times -join ' / ')  median=$(($times | Sort-Object)[1])"
```

### 0.5 Revert the harness

```powershell
git checkout -- src/main.rs
```

Confirm `git status --short` shows `src/main.rs` clean before handing off to implementation. **Record A/B/C numbers in the metrics table.**

---

## Phase 1 — Fix #1: 64 KiB PTY read buffer

### Task 1: Widen the reader buffer

**Files:**
- Modify: `src/terminal.rs:615`

**Interfaces:**
- Consumes: nothing.
- Produces: nothing (no signature change; purely internal to the reader thread in `Session::spawn_with`).

> **No unit-test seam:** the read loop lives on a detached thread reading a live PTY; there is no pure seam to assert a buffer size. The regression guard is the existing PTY tests staying green (proving no semantic change) plus the Phase 3 Scenario-C drain metric (proving the win). This is a deliberate no-seam note, not an omission.

- [ ] **Step 1: Make the change**

In `src/terminal.rs:615`, change:

```rust
            let mut buf = [0u8; 8192];
```

to:

```rust
            // 64 KiB: read() returns whatever is already available (never waits
            // to fill), so this only cuts chunk count under flood — fewer
            // to_vec()/channel/repaint/parse-setup ops per MiB. No latency or
            // ordering change for small output.
            let mut buf = [0u8; 65536];
```

- [ ] **Step 2: Build**

Run:
```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 5
```
Expected: `Finished` with no new warnings.

- [ ] **Step 3: Run the PTY tests (semantic-equivalence guard)**

Run:
```powershell
cargo test terminal:: 2>&1 | Select-Object -Last 8
```
Expected: all pass (spawn/pump/exit tests exercise the read loop end-to-end).

- [ ] **Step 4: Commit**

```powershell
git add src/terminal.rs
git commit -m @'
perf(terminal): widen PTY read buffer 8KiB->64KiB

read() returns available bytes without waiting to fill, so a larger
buffer only reduces chunk count under flood — fewer per-chunk to_vec()
allocs, channel pushes, repaint wakes, and parser/scanner call-setups
per MiB. No latency or byte-ordering change.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
'@
```

---

## Phase 2 — Fix #2: Session-level galley cache

### Task 2a: Split `frame::plan` into `text_rows` + `overlays` (pure refactor, no behavior change)

**Files:**
- Modify: `src/frame.rs` (replace `plan` + `FramePlan` with `text_rows` + `overlays` + `Overlays`; retarget the existing `mod tests`)
- Modify: `src/terminal.rs` `Session::show` (`src/terminal.rs:1006`) to call the two new functions instead of `plan`

**Interfaces:**
- Produces:
  - `pub fn text_rows(grid: &Grid<Cell>, metrics: &CellMetrics) -> Vec<Vec<StyleRun>>`
  - `pub struct Overlays { pub highlights: Vec<egui::Rect>, pub caret: Option<egui::Rect>, pub thumb: Option<egui::Rect>, pub scrolled_back: bool }`
  - `pub fn overlays(grid: &Grid<Cell>, metrics: &CellMetrics, selection: Option<SelRange>, cursor: CursorDraw) -> Overlays`
- Consumes: existing `StyleRun`, `SelRange`, `CursorDraw`, `glyph_style`, `CellMetrics`, geom helpers already imported in `frame.rs`.

- [ ] **Step 1: Replace `plan` + `FramePlan` with the two split functions**

In `src/frame.rs`, delete the `pub fn plan(...) -> FramePlan { ... }` function and the `pub struct FramePlan { ... }` definition, and add:

```rust
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
```

- [ ] **Step 2: Retarget the existing `frame.rs` tests**

The tests currently call `plan(grid, &m, sel, cur)` and assert on `FramePlan` fields. Apply this mechanical transform to each test in `src/frame.rs`'s `mod tests` (the bodies are unchanged except the call + the field access):

| Test | New call | Assert on |
|---|---|---|
| `plan_batches_cells_into_style_runs` | `text_rows(grid, &m)` | returned `Vec` directly (was `.rows`) |
| `plan_renders_nul_cells_as_spaces` | `text_rows(grid, &m)` | returned `Vec` (was `.rows`) |
| `plan_walks_only_cached_dims_when_grid_is_larger` | `text_rows(grid, &m)` | returned `Vec` (was `.rows`) |
| `plan_clamps_stale_metrics_to_grid_bounds` | `text_rows(grid, &m)` | returned `Vec` (was `.rows`) |
| `plan_clamps_selection_beyond_grid` | `overlays(grid, &m, sel, cur)` | `.highlights` |
| `multi_row_selection_spans_match_span_rect` | `overlays(grid, &m, sel, cur)` | `.highlights` |
| `caret_none_when_scrolled_back` | `overlays(grid, &m, sel, cur)` | `.caret` |
| `caret_present_matches_geom_caret_rect` | `overlays(grid, &m, sel, cur)` | `.caret` |
| `thumb_none_without_history` | `overlays(grid, &m, sel, cur)` | `.thumb` |
| `thumb_some_with_history_and_bottom_is_not_scrolled_back` | `overlays(grid, &m, sel, cur)` | `.thumb` / `.scrolled_back` |

Worked example — `plan_batches_cells_into_style_runs`, change:

```rust
        let p = plan(&term, &m, None, CursorDraw::Hidden);
        let runs = &p.rows[0];
```
to:
```rust
        let rows = text_rows(&term, &m);
        let runs = &rows[0];
```

Worked example — `caret_none_when_scrolled_back`, change:

```rust
        let p = plan(&term, &m, None, cur);
        assert!(p.caret.is_none());
```
to:
```rust
        let o = overlays(&term, &m, None, cur);
        assert!(o.caret.is_none());
```

(If any test used `CursorDraw::Hidden`/other variants only for the row path, drop the now-unused `cur`/`sel` args when calling `text_rows`.)

- [ ] **Step 3: Rewire `Session::show` to the two functions (still no caching yet)**

In `src/terminal.rs`, `Session::show`, replace the block that begins `let plan = crate::frame::plan(...)` (around `src/terminal.rs:1159`) down through `painter.galley(rect.min, galley, FG);` (around `src/terminal.rs:1193`) with:

```rust
        let overlays = crate::frame::overlays(self.term.grid(), &metrics, sel, cursor_draw);

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, egui::CornerRadius::ZERO, BG);

        // Text: one TextFormat append per style run, a newline after each row.
        let rows = crate::frame::text_rows(self.term.grid(), &metrics);
        let mut job = LayoutJob::default();
        job.wrap.max_width = f32::INFINITY;
        for runs in &rows {
            for r in runs {
                let st = r.style;
                let line = |on: bool| {
                    if on {
                        egui::Stroke::new(1.0, st.fg)
                    } else {
                        egui::Stroke::NONE
                    }
                };
                job.append(
                    &r.text,
                    0.0,
                    egui::TextFormat {
                        font_id: egui::FontId::monospace(font_px),
                        color: st.fg,
                        background: st.bg.unwrap_or(egui::Color32::TRANSPARENT),
                        underline: line(st.underline),
                        strikethrough: line(st.strikethrough),
                        ..Default::default()
                    },
                );
            }
            job.append("\n", 0.0, egui::TextFormat::default());
        }
        let galley = painter.layout_job(job);
        painter.galley(rect.min, galley, FG);
```

Then update the three later references in the same function: `plan.highlights` → `overlays.highlights` (around `src/terminal.rs:1262`), `plan.caret` → `overlays.caret` (around `src/terminal.rs:1267`), and `plan.thumb` / `plan.scrolled_back` → `overlays.thumb` / `overlays.scrolled_back` (around `src/terminal.rs:1273-1274`).

- [ ] **Step 4: Build + test (proves the split is behavior-identical)**

Run:
```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo test frame:: 2>&1 | Select-Object -Last 8
cargo build 2>&1 | Select-Object -Last 5
```
Expected: all `frame::` tests pass; build clean, no new warnings.

- [ ] **Step 5: Commit**

```powershell
git add src/frame.rs src/terminal.rs
git commit -m @'
refactor(frame): split plan into cached text_rows + per-frame overlays

Separates the expensive content-only cell walk (text_rows) from the
cheap per-frame overlays (selection highlights, caret, thumb) so the
next commit can memoize the galley built from text_rows without
touching the per-frame overlay paint. Pure refactor; frame tests
retargeted, no behavior change.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
'@
```

### Task 2b: Add `content_gen` — the grid-content version signal

**Files:**
- Modify: `src/terminal.rs` (add `content_gen` field + init + two bump sites)
- Test: `src/terminal.rs` `mod tests`

**Interfaces:**
- Produces: `Session.content_gen: u64` — monotonic, bumped on every grid-content mutation. Consumed by Task 2c's galley key.

- [ ] **Step 1: Write the failing test**

Add to `src/terminal.rs` `mod tests`:

```rust
    #[test]
    fn content_gen_bumps_on_injected_note() {
        // The dispatch banner is written straight to the emulator (NOT the PTY),
        // so it never rides pump(). The galley cache must still invalidate — this
        // is the one grid mutation output_gen deliberately does not cover.
        let ctx = egui::Context::default();
        let mut s = Session::spawn(Shell::PowerShell, None, &[], ctx).expect("spawn");
        let before = s.content_gen;
        s.inject_note("dispatched: test");
        s.resize(40, 10); // first resize flushes the pending note into the grid
        assert!(
            s.content_gen > before,
            "note injection must bump content_gen (before={before}, after={})",
            s.content_gen
        );
    }
```

- [ ] **Step 2: Run it — verify it fails to compile**

Run:
```powershell
cargo test content_gen_bumps_on_injected_note 2>&1 | Select-Object -Last 8
```
Expected: compile error `no field content_gen on type &Session`.

- [ ] **Step 3: Add the field, its init, and both bump sites**

In the `Session` struct (`src/terminal.rs`, near the `output_gen` field), add after `output_gen: u64,` and its doc comment:

```rust
    // Grid-content version for the render galley cache. Distinct from
    // output_gen (which means "child produced PTY bytes" and drives settle
    // quiescence): content_gen bumps on EVERY grid-content mutation, including
    // the inject_note banner that never rides pump(). Single source of truth
    // for "the galley is stale" — bump it wherever self.term's grid changes.
    content_gen: u64,
```

In `Session::spawn_with`, in the `Ok(Session { ... })` initializer, add after `output_gen: 0,`:

```rust
            content_gen: 0,
```

In `pump` (`src/terminal.rs:825`), immediately after `self.output_gen = self.output_gen.wrapping_add(1);`:

```rust
            self.content_gen = self.content_gen.wrapping_add(1);
```

In `resize`, in the `pending_note` flush branch, immediately after `self.parser.advance(&mut self.term, &bytes);`:

```rust
            self.content_gen = self.content_gen.wrapping_add(1);
```

- [ ] **Step 4: Run the test — verify it passes**

Run:
```powershell
cargo test content_gen_bumps_on_injected_note 2>&1 | Select-Object -Last 8
```
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/terminal.rs
git commit -m @'
feat(terminal): content_gen — grid-content version for the galley cache

A monotonic counter bumped on every grid-content mutation (pump plus the
inject_note flush in resize). Kept separate from output_gen so the settle
machinery's PTY-freshness signal is not perturbed by a foreman-injected
banner. Consumed next by the show() galley memo.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
'@
```

### Task 2c: Memoize the galley in `Session::show`

**Files:**
- Modify: `src/terminal.rs` (add `GalleyKey` type + `galley_cache` field + init + the memo in `show`)

**Interfaces:**
- Consumes: `Session.content_gen` (Task 2b), `frame::text_rows` (Task 2a).
- Produces: no public signature change — `show` renders identically, just skips the row walk + `LayoutJob` build + `layout_job` on an unchanged pane.

- [ ] **Step 1: Add the key type and cache field**

In `src/terminal.rs`, add near the top-level type declarations (e.g. just above `pub struct Session`):

```rust
/// Cache key for a pane's rendered galley. All five inputs fully determine the
/// laid-out text: content version, scroll position, grid dims, and font size.
/// Selection/caret are NOT here — they paint as separate overlays.
#[derive(Clone, Copy, PartialEq, Eq)]
struct GalleyKey {
    content_gen: u64,
    off: usize,
    cols: usize,
    rows: usize,
    font_bits: u32,
}
```

In the `Session` struct, add after the `content_gen` field:

```rust
    // Memoized text galley + the key it was built for. On a key hit show()
    // re-clones the Arc (cheap) instead of re-walking the grid and rebuilding
    // the LayoutJob. Invalidated implicitly by any key change.
    galley_cache: Option<(GalleyKey, std::sync::Arc<egui::Galley>)>,
```

In `Session::spawn_with`'s initializer, after `content_gen: 0,`:

```rust
            galley_cache: None,
```

- [ ] **Step 2: Replace the unconditional galley build with the memo**

In `Session::show` (from Task 2a Step 3), replace the block:

```rust
        // Text: one TextFormat append per style run, a newline after each row.
        let rows = crate::frame::text_rows(self.term.grid(), &metrics);
        let mut job = LayoutJob::default();
        job.wrap.max_width = f32::INFINITY;
        for runs in &rows {
            for r in runs {
                let st = r.style;
                let line = |on: bool| {
                    if on {
                        egui::Stroke::new(1.0, st.fg)
                    } else {
                        egui::Stroke::NONE
                    }
                };
                job.append(
                    &r.text,
                    0.0,
                    egui::TextFormat {
                        font_id: egui::FontId::monospace(font_px),
                        color: st.fg,
                        background: st.bg.unwrap_or(egui::Color32::TRANSPARENT),
                        underline: line(st.underline),
                        strikethrough: line(st.strikethrough),
                        ..Default::default()
                    },
                );
            }
            job.append("\n", 0.0, egui::TextFormat::default());
        }
        let galley = painter.layout_job(job);
        painter.galley(rect.min, galley, FG);
```

with:

```rust
        // Text galley — rebuilt only when the content/scroll/dims/font key
        // changes; otherwise a cheap Arc clone. Selection + caret are overlays
        // (below), so they never invalidate this.
        let key = GalleyKey {
            content_gen: self.content_gen,
            off: self.term.grid().display_offset(),
            cols: self.cols,
            rows: self.rows,
            font_bits: font_px.to_bits(),
        };
        let galley = match &self.galley_cache {
            Some((k, g)) if *k == key => g.clone(),
            _ => {
                let rows = crate::frame::text_rows(self.term.grid(), &metrics);
                let mut job = LayoutJob::default();
                job.wrap.max_width = f32::INFINITY;
                for runs in &rows {
                    for r in runs {
                        let st = r.style;
                        let line = |on: bool| {
                            if on {
                                egui::Stroke::new(1.0, st.fg)
                            } else {
                                egui::Stroke::NONE
                            }
                        };
                        job.append(
                            &r.text,
                            0.0,
                            egui::TextFormat {
                                font_id: egui::FontId::monospace(font_px),
                                color: st.fg,
                                background: st.bg.unwrap_or(egui::Color32::TRANSPARENT),
                                underline: line(st.underline),
                                strikethrough: line(st.strikethrough),
                                ..Default::default()
                            },
                        );
                    }
                    job.append("\n", 0.0, egui::TextFormat::default());
                }
                let g = painter.layout_job(job);
                self.galley_cache = Some((key, g.clone()));
                g
            }
        };
        painter.galley(rect.min, galley, FG);
```

- [ ] **Step 3: Build + full test run**

Run:
```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 5
cargo test 2>&1 | Select-Object -Last 8
```
Expected: build clean (no new warnings), all tests pass.

- [ ] **Step 4: Visual acid test (the memo must not render stale)**

Run the release exe and drive real programs in a pane; confirm no stale/frozen text and correct updates:
```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo run --release
```
Manually in a pane: run `dir`, hold a key to scroll, run `vim` or `lazygit` if available, resize the window, Ctrl+Scroll to zoom. Every case must update live (no one-frame-stale text, no ghosting on resize/zoom/scroll). Close the app when satisfied.

- [ ] **Step 5: Commit**

```powershell
git add src/terminal.rs
git commit -m @'
perf(terminal): memoize the per-pane text galley

show() now rebuilds the LayoutJob/galley only when the content version,
scroll offset, grid dims, or font size change; otherwise it re-clones the
cached Arc<Galley>. Kills the redundant per-cell walk + O(chars) layout
hash that every visible pane paid every frame — an unchanged pane now
costs ~one Arc clone, so one busy pane no longer taxes the other panes'
draw time. Selection/caret/thumb stay per-frame overlays.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
'@
```

---

## Phase 3 — Post metrics (orchestrator-run, AFTER Phase 2)

Re-add the same `[DEBUG-perf]` harness (Phase 0.1), rebuild release, and re-run Scenarios A, B, C **verbatim** (0.2–0.4). Then `git checkout -- src/main.rs` (0.5). Fill the "After" column and confirm:

- **Scenario B (the #2 metric):** avg/p95 show-ms should drop substantially — the 11 static panes now cost ~one Arc clone each instead of a full galley rebuild. This is the proof for #2.
- **Scenario C (the #1 metric):** median flood-drain ms should drop — fewer chunks × per-chunk overhead. This is the proof for #1.
- **Scenario A (idle floor):** should be unchanged or slightly better (idle already rebuilt little; the cache makes the 10 Hz idle repaints ~free). Must not regress.

If B/C don't improve, **do not claim success** — investigate (was the flood pane actually flooding? is `content_gen` invalidating too aggressively so the cache never hits? add a temp hit/miss counter to `show` and re-measure).

## Metrics table

| Scenario | Metric | Before (HEAD) | After (fixes) |
|---|---|---|---|
| A — idle, 1 pane | avg / max show-ms | 0.155 / 2.86 (n=221) | **0.072** / 2.97 (n=211) — **2.1× faster** |
| B — 12 panes, 1 flood/11 static | avg / p95 / max show-ms | 0.433 / 0.506 / 50.44 (n=3375) | **0.317 / 0.357** / 43.34 (n=3302) — **~27% avg, ~30% p95** |
| C — 1-pane flood drain | median ms (of 3) | 7505 (7505/7240/7514) | 7352 (7607/7062/7352) — ~2%, within noise |

Baseline captured 2026-07-04 on HEAD (ConPTY 1.24 present, no perf fixes), release build, 200k-line/11.3 MB flood file. After = all four commits on `perf/render-read-hotpath`.

**Verdict:** #2 (galley cache) is the clear win — idle draw 2.1× faster, 12-pane render ~27–30% cheaper. #1 (64 KiB buffer) did **not** move the flood-drain wall-clock (~2%, noise): that path is bound by per-byte parse + ConPTY throughput, not the per-chunk overhead the buffer trims. #1 is retained as a sound zero-risk change (fewer syscalls/allocs/wakes per MiB) but is not the source of the win. Galley-memo correctness verified by the pixel acid test (echo ALPHA→BRAVO both painted, no stale frame).

## Self-review checklist (done during authoring)

- **Coverage:** #1 → Task 1. #2 → Tasks 2a (seam), 2b (invalidation signal), 2c (memo). Baseline → Phase 0. Proof → Phase 3. ✓
- **Correctness guards from the review encoded:** the `output_gen` bump gap is fixed as a dedicated `content_gen` with a bump in *both* mutation sites (2b), and the note-flush site has a deterministic regression test. ✓
- **Type consistency:** `text_rows`/`overlays`/`Overlays`/`GalleyKey`/`content_gen`/`galley_cache` names are identical across 2a→2b→2c. ✓
- **No new deps, no VoidListener, GNU build, kill-before-build, harness never committed** — in Global Constraints and each build step. ✓
