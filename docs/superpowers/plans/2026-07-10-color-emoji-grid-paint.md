# Color Emoji + Grid-Locked Terminal Paint — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Paint every terminal cell on the grid (`col × cell_w`) so the caret tracks text after emoji/CJK, and stamp multi-color single-codepoint emoji from Windows color fonts — paint-only, fail-open, with zero layout work on unchanged frames.

**Architecture:** Deepen the existing **Frame plan** seam (`src/frame.rs`) with `plan_paint` → grid-locked glyphs + `emoji_sites`. `Session::show` caches positioned mono shapes under a `GalleyKey`-equivalent key (0 layout calls when the key is unchanged). New **EmojiAtlas** (`src/emoji_raster.rs`): small interface `color_glyph(ch, px) -> Option<RgbaImage>`; DirectWrite prod adapter + fake test adapter; stamps are a **separate** overlay cache that must not bust mono memoization. Spec: `docs/superpowers/specs/2026-07-10-color-emoji-grid-paint-design.md`.

**Tech Stack:** Rust, egui/epaint 0.34, alacritty_terminal 0.26, Windows DirectWrite (phase 3 only). No new crates for phases 1–2. Phase 3 may use `windows` crate APIs already pulled transitively via `windows-sys` if possible; prefer minimal new deps — ask before adding.

## Global Constraints

- Spec invariants: **paint-only** (no PTY width hacks, no grid spacer edits, no DSR lies).
- **Unchanged frame ⇒ 0 layout calls** for pane text (phase 1 gate). Layout call = any terminal-paint path into egui/epaint text shaping that builds a new `Galley` (instrument `layout_no_wrap` / `layout_job` used by Session paint; plan uses a testable counter).
- Mono glyph wider than `cell_w` **overhangs** neighbor; never shift columns.
- Emoji stamps only when **default emoji presentation** (`Emoji_Presentation=Yes`); text defaults and VS15/VS16 → mono in v1.
- Stamp cache **separate** from mono plan cache; atlas miss must not force mono re-layout every frame.
- Windows, GNU toolchain. Kill app before link:  
  `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500`  
  (never when `FOREMAN=1` — use `cargo build --target-dir target/agent` instead).
- Bin crate: `cargo test <filter>` — not `--lib`.
- Stage by name; `type(scope): subject` + trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Never `VoidListener` on live Sessions.

## File map

| File | Responsibility |
|------|----------------|
| `src/frame.rs` | `GlyphPlacement`, `EmojiSite`, `plan_paint`; pure tests |
| `src/terminal.rs` | Cache key, layout counter hook, grid-locked paint, stamp replay |
| `src/emoji_raster.rs` | Atlas interface, fake, DirectWrite (phase 3) |
| `src/geom.rs` | Reuse `cell_rect` / `span_rect` only |
| `src/main.rs` | `mod emoji_raster;` |
| `docs/font-fallback.md` | Note color stamps when phase 3 lands |

## Phases ↔ tasks

| Phase | Tasks | Done when |
|-------|-------|-----------|
| 1 Grid mono + perf | 1–3 | Caret aligned; 0 layout on unchanged frame |
| 2 emoji_sites | 4 | Sites pure-tested; still mono paint |
| 3 Color stamps | 5–7 | Multi-color single-codepoint emoji |
| Docs | 8 | Feature doc updated |

---

### Task 1: Pure `plan_paint` — grid-locked glyphs

**Files:**
- Modify: `src/frame.rs`
- Test: same file `#[cfg(test)]`

**Interfaces:**
- Consumes: existing `text_rows` helpers / grid walk patterns; `CellMetrics`; `glyph_style`
- Produces:
  ```rust
  pub struct GlyphPlacement {
      pub row: usize,
      pub col: usize,
      pub ch: char,
      pub style: GlyphStyle,
      pub width_cells: u8, // 1 or 2
  }
  pub struct EmojiSite {
      pub row: usize,
      pub col: usize,
      pub ch: char,
      pub width_cells: u8,
  }
  pub struct PaintPlan {
      pub glyphs: Vec<GlyphPlacement>,
      pub emoji_sites: Vec<EmojiSite>, // empty until Task 4
  }
  pub fn plan_paint(grid: &Grid<Cell>, metrics: &CellMetrics) -> PaintPlan
  ```
- Keep `text_rows` until Task 3 removes its use (or implement `plan_paint` and leave `text_rows` calling it for batching tests — prefer: `plan_paint` is the new primary; `text_rows` can remain for old tests until show migrates).

- [ ] **Step 1: Write failing tests** in `src/frame.rs` tests module:

```rust
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
        // 你好 is two width-2 CJK glyphs when the terminal classifies them wide.
        let term = term_with("你好".as_bytes(), 8, 1);
        let m = metrics(8, 1);
        let plan = plan_paint(term.grid(), &m);
        let non_space: Vec<_> = plan
            .glyphs
            .iter()
            .filter(|g| g.ch != ' ' && g.ch != '\0')
            .cloned()
            .collect();
        // Expect two logical glyphs at cols 0 and 2, each width_cells == 2.
        // If the fixture shell doesn't mark wide, skip with a clear assert message.
        assert!(
            non_space.len() >= 2,
            "expected CJK cells; got {non_space:?}"
        );
        let wides: Vec<_> = non_space.iter().filter(|g| g.width_cells == 2).collect();
        if wides.is_empty() {
            // Grid didn't flag WIDE_CHAR — still must not emit spacer as its own char.
            assert!(
                plan.glyphs.iter().all(|g| {
                    !term.grid()[alacritty_terminal::index::Line(0)]
                        [alacritty_terminal::index::Column(g.col)]
                        .flags
                        .contains(alacritty_terminal::term::cell::Flags::WIDE_CHAR_SPACER)
                        || g.ch == ' ' || g.ch == '\0'
                }) == false
                    || plan
                        .glyphs
                        .iter()
                        .filter(|g| {
                            term.grid()[alacritty_terminal::index::Line(0)]
                                [alacritty_terminal::index::Column(g.col)]
                                .flags
                                .contains(alacritty_terminal::term::cell::Flags::WIDE_CHAR_SPACER)
                        })
                        .count()
                        == 0
            );
        } else {
            assert_eq!(wides[0].col, 0);
            assert_eq!(wides[0].width_cells, 2);
            assert_eq!(wides[1].col, 2);
        }
        // No placement that is only a WIDE_CHAR_SPACER with a real char
        for g in &plan.glyphs {
            let cell = &term.grid()[alacritty_terminal::index::Line(0 as i32)]
                [alacritty_terminal::index::Column(g.col)];
            if cell
                .flags
                .contains(alacritty_terminal::term::cell::Flags::WIDE_CHAR_SPACER)
            {
                panic!("spacer cell must not appear as a GlyphPlacement: {g:?}");
            }
        }
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
```

Simplify the wide-char test if the double-negative is messy — intent: **WIDE_CHAR_SPACER cells never become `GlyphPlacement`s**; wide base cell has `width_cells == 2`.

- [ ] **Step 2: Run RED**

```powershell
cargo test plan_paint_ 2>&1 | Select-Object -Last 25
```

Expected: compile error — `plan_paint` / types missing.

- [ ] **Step 3: Implement minimal `plan_paint`**

Walk like `text_rows`, but per cell:

```rust
use alacritty_terminal::term::cell::Flags;

// inside row/col loop after clamp:
let cell = &grid[Line(row as i32 - off)][Column(col)];
if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
    continue;
}
let width_cells = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
let ch = if cell.c == '\0' { ' ' } else { cell.c };
glyphs.push(GlyphPlacement {
    row,
    col,
    ch,
    style: glyph_style(cell.flags, cell.fg, cell.bg),
    width_cells,
});
// emoji_sites: Vec::new() for now
```

- [ ] **Step 4: GREEN**

```powershell
cargo test plan_paint_ 2>&1 | Select-Object -Last 20
```

- [ ] **Step 5: Commit**

```text
feat(frame): plan_paint with grid-locked glyph placements

Pure walk skips WIDE_CHAR_SPACER; width_cells 1|2 from WIDE_CHAR flag.
emoji_sites empty until detector task. Foundation for caret-aligned paint.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
```

---

### Task 2: Layout-call counter (test seam)

**Files:**
- Modify: `src/terminal.rs` (or tiny `src/paint_stats.rs` if cleaner — prefer terminal-local for one counter)

**Interfaces:**
- Produces:
  ```rust
  // test + debug only
  fn layout_call_count() -> u64
  fn reset_layout_call_count()
  fn note_layout_call() // called at every Session paint path that creates a Galley
  ```
  Use `AtomicU64` + `Relaxed`, or `thread_local!` — process-wide is fine for unit tests that run serially on paint helpers.

- [ ] **Step 1: Failing test** — after a pure helper that will wrap layout:

```rust
    #[test]
    fn layout_counter_increments_when_noted() {
        reset_layout_call_count();
        assert_eq!(layout_call_count(), 0);
        note_layout_call();
        note_layout_call();
        assert_eq!(layout_call_count(), 2);
        reset_layout_call_count();
        assert_eq!(layout_call_count(), 0);
    }
```

- [ ] **Step 2: RED** then implement atomics; **GREEN**; commit:

```text
test(terminal): layout_call_count seam for paint perf gate

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
```

---

### Task 3: Wire grid-locked mono paint + memo cache

**Files:**
- Modify: `src/terminal.rs` `Session::show` text paint block (~1263–1309)
- Keep: `GalleyKey` fields (or rename to `PaintKey` with same fields)

**Interfaces:**
- Consumes: `plan_paint`, `GlyphPlacement`, `layout_call_count`
- Cache value: whatever avoids re-layout — e.g. `Arc<egui::Galley>` built from a single `LayoutJob` that uses **leading spaces / tab-less fixed placement**:

**Preferred paint strategy (pick one; document in commit):**

**Strategy P (recommended):** For each `GlyphPlacement`, call `painter.layout_no_wrap(ch.to_string(), font, color)` **only when building the cache entry**, store `Vec<(egui::Pos2, Arc<Galley>)>` (or one big custom mesh). On cache hit: only `painter.galley(pos, g, …)` — **no** `layout_*`. Call `note_layout_call()` once per `layout_no_wrap` / `layout_job` during rebuild.

**Strategy Q (alternative):** One `LayoutJob` per row with each glyph as its own section and `extra_letter_spacing` hacks — usually wrong; avoid unless P is too slow on rebuild.

Position: `metrics.cell_rect(row, col).min` (or baseline-adjust if needed so mono baseline matches today — visual check).

Overhang: do not clip; allow galley wider than `cell_w`.

- [ ] **Step 1: Integration-style unit test** (no GUI window if possible): extract rebuild into a function testable with a mock painter is hard — instead:

```rust
    #[test]
    fn paint_cache_second_build_does_zero_layouts() {
        // Build a fake plan twice through the same Session-level helper.
        // If the helper is `fn rebuild_mono_paint(...)` and `fn paint_mono_cached(...)`,
        // call rebuild once (N layout notes), then paint path with same key (0 new notes).
        reset_layout_call_count();
        // ... construct Session or free function that takes PaintPlan + key ...
        // First miss: layout_call_count() > 0
        // Reset counter; second hit with same key: layout_call_count() == 0
    }
```

Implement a small pure-ish helper on Session:

```rust
struct MonoPaintCache {
    key: GalleyKey, // reuse existing struct
    items: Arc<Vec<MonoGlyph>>, // pos + Arc<Galley> + style extras
}
struct MonoGlyph {
    pos: egui::Pos2,
    galley: Arc<egui::Galley>,
    // fg already in galley; underline may need separate strokes from style
}
```

Underlines/strikethrough: either bake into TextFormat when laying out, or draw strokes from placement style using `cell_rect` (prefer TextFormat like today).

- [ ] **Step 2: Replace free-flow whole-pane `LayoutJob` loop** with:
  1. `let plan = frame::plan_paint(...)`
  2. key from content_gen, off, cols, rows, font_bits
  3. if cache hit → blit items
  4. else rebuild from `plan.glyphs`, `note_layout_call` per layout, store cache

- [ ] **Step 3: Keep `text_rows` tests green**; if show no longer uses `text_rows`, leave function for now or delete in a cleanup commit only if nothing calls it.

- [ ] **Step 4: Manual check**

```powershell
# build and run; type ASCII; paste 你好; caret should track
cargo build 2>&1 | Select-Object -Last 15
```

- [ ] **Step 5: Commit**

```text
feat(terminal): grid-locked mono paint with GalleyKey memo

Replace free-flow whole-pane LayoutJob with per-placement layout
cached under content_gen/scroll/dims/font. Unchanged frames re-blit
only (0 layout_*). Overhang allowed; no column reflow.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
```

---

### Task 4: `emoji_sites` detector (phase 2)

**Files:**
- Modify: `src/frame.rs` `plan_paint`
- Optional: `fn is_default_emoji_presentation(ch: char) -> bool` in frame or emoji_raster

**Rules (spec):**
- Single scalar only.
- Require **default emoji presentation**. Minimal v1: use Unicode ranges that are Emoji_Presentation=Yes (e.g. most of U+1F300–U+1FAFF, U+1F600–U+1F64F, etc.) — implement as a pure function with table/ranges + unit tests for:
  - `🥒` U+1F952 → true
  - `☁` U+2601 → false
  - `A` → false
- Do **not** treat VS16 sequences in v1 (no combining).
- Only emit `EmojiSite` when base cell is WIDE or the classifier says emoji **and** width_cells matches model (if width is 1 and text presentation, skip stamp site even if range matches).

```rust
// When pushing a GlyphPlacement that is_default_emoji_presentation(ch) && width_cells >= 1:
// Prefer: width_cells == 2 for stamp candidates (emoji typically wide).
// If width_cells == 1, skip emoji_sites (text presentation / ambiguous).
if is_default_emoji_presentation(ch) && width_cells == 2 {
    emoji_sites.push(EmojiSite { row, col, ch, width_cells });
}
```

- [ ] **Step 1: Tests**

```rust
    #[test]
    fn cucumber_is_default_emoji_presentation() {
        assert!(is_default_emoji_presentation('🥒'));
    }
    #[test]
    fn cloud_text_default_is_not_emoji_presentation() {
        assert!(!is_default_emoji_presentation('☁'));
    }
    #[test]
    fn plan_paint_emits_emoji_site_for_wide_emoji() {
        let term = term_with("🥒".as_bytes(), 8, 1);
        let m = metrics(8, 1);
        let plan = plan_paint(term.grid(), &m);
        // If ConPTY/alacritty in unit test marks wide:
        if plan.glyphs.iter().any(|g| g.width_cells == 2 && g.ch == '🥒') {
            assert_eq!(plan.emoji_sites.len(), 1);
            assert_eq!(plan.emoji_sites[0].ch, '🥒');
            assert_eq!(plan.emoji_sites[0].width_cells, 2);
        }
    }
```

Unit tests that feed UTF-8 into alacritty `Term` (like existing `term_with`) should mark emoji wide — verify; if not, still test classifier in isolation.

- [ ] **Step 2: RED → implement → GREEN → commit**

```text
feat(frame): emoji_sites for default-presentation wide scalars

Classifier prefers Emoji_Presentation=Yes; text defaults like ☁ stay
mono-only. Sites do not yet change paint (phase 3 stamps).

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
```

---

### Task 5: EmojiAtlas interface + fake (phase 3 prep)

**Files:**
- Create: `src/emoji_raster.rs`
- Modify: `src/main.rs` — `mod emoji_raster;`

**Interfaces:**

```rust
pub trait EmojiRaster: Send {
    fn color_glyph(&mut self, ch: char, px: u32) -> Option<image::RgbaImage>;
    // Prefer no new dep: return Vec<u8> RGBA + (w,h):
}

pub struct RgbaGlyph {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>, // w*h*4
}

pub trait EmojiRaster: Send {
    fn color_glyph(&mut self, ch: char, px: u32) -> Option<RgbaGlyph>;
}

pub struct FakeEmojiRaster {
    pub map: std::collections::HashMap<char, RgbaGlyph>,
}

impl EmojiRaster for FakeEmojiRaster {
    fn color_glyph(&mut self, ch: char, px: u32) -> Option<RgbaGlyph> {
        let _ = px;
        self.map.get(&ch).cloned()
    }
}
```

- [ ] **Step 1: Tests**

```rust
    #[test]
    fn fake_returns_fixture() {
        let g = RgbaGlyph { w: 1, h: 1, rgba: vec![0, 255, 0, 255] };
        let mut fake = FakeEmojiRaster { map: [ ('🥒', g) ].into_iter().collect() };
        let got = fake.color_glyph('🥒', 16).unwrap();
        assert_eq!(got.rgba, vec![0, 255, 0, 255]);
        assert!(fake.color_glyph('A', 16).is_none());
    }
```

- [ ] **Step 2: Implement + commit**

```text
feat(emoji): EmojiRaster trait + FakeEmojiRaster

Second adapter makes the raster seam real for tests. DirectWrite next.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
```

---

### Task 6: DirectWrite (or Windows) color raster adapter

**Files:**
- Modify: `src/emoji_raster.rs`

**Interfaces:**
- `pub struct DirectWriteEmojiRaster { ... }`
- `impl EmojiRaster for DirectWriteEmojiRaster`
- `pub fn system_emoji_raster() -> Box<dyn EmojiRaster>` — DWrite if available, else empty always-None stub

**Implementation notes (spike-friendly):**
- Load Segoe UI Emoji; request color glyph bitmap at `px`.
- On any failure → `None`.
- Do not panic.
- If DWrite color glyph API is too heavy for first landing: ship always-None prod adapter behind `system_emoji_raster` **only after** Task 5 tests pass, and open a follow-up — but spec requires color for done bar; spike must land real bitmaps for at least one codepoint.

- [ ] **Step 1:** Manual/dev test or `#[cfg(windows)]` test ignored by default:

```rust
    #[test]
    #[ignore] // needs font; run: cargo test dwrite_cucumber -- --ignored --nocapture
    fn dwrite_cucumber_nonzero() {
        let mut r = DirectWriteEmojiRaster::new().expect("dwrite");
        let g = r.color_glyph('🥒', 32).expect("glyph");
        assert!(g.w > 0 && g.h > 0);
        assert_eq!(g.rgba.len(), (g.w * g.h * 4) as usize);
        // not all zeros
        assert!(g.rgba.iter().any(|&b| b != 0));
    }
```

- [ ] **Step 2: Implement → commit**

```text
feat(emoji): DirectWrite color glyph rasterizer

Fail-open None on missing font/API. system_emoji_raster() for GUI wiring.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
```

---

### Task 7: Stamp paint in `Session::show` (separate cache)

**Files:**
- Modify: `src/terminal.rs`

**Rules:**
- Hold `Box<dyn EmojiRaster>` on Session or App (prefer **one shared** on App/`Context` data if easy; else per-Session).
- Texture cache: `HashMap<(char, u32), egui::TextureHandle>` (px includes font-derived size).
- For each `plan.emoji_sites` after mono paint:
  - if texture cached → image
  - else `color_glyph` → load_texture → cache → image
  - `None` → leave mono
- Skip mono for that span when stamp succeeds (rebuild mono cache **without** those glyphs, **or** draw stamps after mono and cover — covering is OK if stamp is opaque).
- **Must not** change mono `PaintKey` when only atlas fills in.

Draw: `painter.image(tex.id(), metrics.span_rect(row, col, col + width_cells as usize - 1), …)` with white tint / full UV.

- [ ] **Step 1: Manual** — paste `🥒`, confirm color; type `abc`, caret OK; idle CPU not spinning.

- [ ] **Step 2: Commit**

```text
feat(terminal): color emoji stamps from EmojiRaster overlay cache

Stamps use span_rect; separate from mono GalleyKey memo. Fail-open mono.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
```

---

### Task 8: Docs

**Files:**
- Modify: `docs/font-fallback.md` — section “Color stamps (optional)”
- Optional: one line in warp-feature-candidates if still open

- [ ] **Step 1: Write** short section: what/why, paint-only, Emoji_Presentation, perf gate pointer to spec.

- [ ] **Step 2: Commit**

```text
docs(ui): color emoji stamps + grid paint notes

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
```

---

## Perf harness (phase 1 gate — orchestrator)

After Task 3, before calling phase 1 done:

1. Counter test: two paint paths same key → second has `layout_call_count() == 0`.
2. Optional: reuse `docs/superpowers/plans/2026-07-04-render-read-perf.md` style `[DEBUG-perf]` **throwaway** show-ms; compare relative to baseline in `docs/followups-latency-and-control.md`. Do not commit debug prints.
3. Report mentally: shapes/vertices if easy; not a hard number gate.

---

## Spec coverage checklist

| Spec requirement | Task |
|------------------|------|
| All-cells grid lock | 1, 3 |
| 0 layout unchanged frame | 2, 3 |
| Overhang no reflow | 3 |
| emoji_sites + Emoji_Presentation | 4 |
| EmojiAtlas + fake + DWrite | 5, 6 |
| Stamps separate cache | 7 |
| Fail-open | 4–7 |
| Paint-only / no PTY | all |
| Docs | 8 |
| Success #6 perf | 2, 3 + harness |

## Out of scope (do not implement in this plan)

- ZWJ / skin tones / VS16 color
- Merging with kitty `graphics.rs`
- Custom GPU UI framework
- Absolute ms perf SLAs

---

## Self-review (plan author)

- No TBD left for implementers: classifier rule, cache separation, counter definition, phases.
- Types consistent: `GlyphPlacement`, `EmojiSite`, `PaintPlan`, `RgbaGlyph`, `EmojiRaster`.
- Phase 1 can ship alone for caret fix if DWrite slips.
