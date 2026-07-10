# Color emoji + grid-locked terminal paint

**Status:** approved design (brainstorming 2026-07-10)  
**Bar:** color emoji **and** caret aligned with the cell grid  
**Approach:** A — full grid paint + emoji texture stamps (paint-only)

## Problem

1. **Mono emoji / tofu path:** egui’s atlas draws Segoe UI Emoji as outline
   shapes (one solid color). Warp-style multi-color pickles need color layers
   or bitmaps, not another `Color32` on a `LayoutJob`.
2. **Caret drift:** `Session::show` paints the grid as one free-flow
   `LayoutJob` galley (natural glyph advances). The caret is painted on
   **Cell metrics** (`col × cell_w` from measuring `"M"`). Emoji/CJK advances
   often ≠ N × cell width, so text and caret desync after paste/type.

Landing-page “colored” wordmark text is **per-glyph solid Color32** (sheen) —
not multi-color emoji. Kitty graphics already stamp **RGBA textures** over the
grid without entering selection/copy. That overlay idea is the right cousin for
color emoji; the free galley is the wrong text layout for a cell terminal.

## Goals / non-goals

### Goals

- Multi-color emoji for **single Unicode scalar** codepoints (e.g. `🥒` `🚀`).
- **All cells** painted on the grid (`col × cell_w`), not only emoji sites —
  fixes caret for emoji **and** CJK.
- Paint-only: ConPTY + alacritty remain the only model for cells/cursor/bytes.
- Fail-open: raster or detect miss → mono glyph from the plan (no panic, no blank pane).
- Deep modules: complexity behind small interfaces; `Session::show` stays thin replay.

### Non-goals (v1)

- ZWJ sequences, skin-tone modifiers as first-class (fall back to mono).
- Color emoji via egui’s text atlas / COLR inside `LayoutJob`.
- Merging emoji into the kitty graphics **protocol** module.
- Changing paste/type/inject encoding, DSR replies, or grid contents to “fix” width.
- Cross-platform raster (Windows DirectWrite / system color font only for v1).
- Matching Warp’s full UI stack.

## Architecture

```
PTY bytes → ConPTY → alacritty grid  (unchanged)
                         │
                         ▼
              plan_paint(grid, metrics)   pure — frame seam
                    │              │
                    ▼              ▼
            mono placements   emoji_sites
            (every cell)      (single codepoints)
                    │              │
                    ▼              ▼
            paint at col×cw   EmojiAtlas.color_glyph
                    │              │
                    └──── show ────┘→ texture stamp in cell/span rect
                         │
                         ▼
              overlays (caret/sel/thumb) + kitty (unchanged)
```

**Invariants**

| Do | Don’t |
|----|--------|
| Paint-only | Rewrite PTY/paste bytes for width |
| Cursor from model + Caret gate | Lie in DSR / fake ConPTY cursor |
| Wide char = model’s 2 cells | Insert/delete spacers in the grid |
| Raster `None` → mono | Panic or blank the pane |
| Overlay ≠ selection text (like kitty) | Put stamp images into copy/snapshot |

## Components (deep modules)

### 1. Frame plan — extend (existing seam)

**Home:** `src/frame.rs` (and callers in `Session::show`).  
**Role:** pure “what to paint where” from grid + `CellMetrics`.

Conceptual interface:

```text
PaintPlan {
  glyphs: ...      // grid-locked: row, col, ch, style, width_cells (1 or 2)
  emoji_sites: Vec<EmojiSite>
  // existing overlay inputs remain (sel, caret draw, thumb) as today
}

EmojiSite { row, col, ch, width_cells }

fn plan_paint(grid, metrics) -> PaintPlan
```

**Rules**

- Clamp walks to the grid’s **real** bounds first (existing frame doctrine —
  stale index panics abort the process).
- One logical wide glyph: base cell holds the character; spacer cell does not
  emit a second stamp or a second full mono double-draw of the emoji.
- `emoji_sites`: v1 classifier for **single scalar** emoji codepoints only.
  ZWJ / complex sequences → no site → mono only.
- **All cells** get grid-locked mono placement info (not “emoji only”).

**Tests:** pure fixtures with `Term`/grid bytes; assert columns and sites.
No GUI, no DirectWrite.

**Depth:** one walk owns clamp, wide handling, classify. Callers do not re-implement.

### 2. EmojiAtlas / `emoji_raster` (new module)

**Home:** new file e.g. `src/emoji_raster.rs` (final name at plan time).  
**Role:** `char + pixel size → color RGBA` (and texture caching at the GUI edge).

Conceptual interface:

```text
fn color_glyph(ch: char, px: u32) -> Option<RgbaImage>
// show (or a thin wrapper) loads egui textures; cache key (ch, px [, dpr])
```

**Invariants callers must know**

- `None` ⇒ paint mono from the plan (fail-open).
- Paint-only; never mutates the grid or PTY.
- v1: single codepoints; everything else → `None`.
- Prod adapter: Windows color font path (DirectWrite / system Segoe UI Emoji color).
- Test adapter: fake that returns a solid or fixture image (second adapter makes the seam real).

**Does not:** know `Session`, ConPTY, or kitty APC.

**Do not merge** into `graphics.rs` (kitty protocol). Same *technique* (RGBA
overlay), different domain. Optional shared texture-cache helper only if
duplication becomes real pain later.

### 3. `Session::show` — thin replay

1. Build `CellMetrics` (existing).
2. `plan = plan_paint(...)`.
3. Paint mono glyphs at `cell_rect` / span rects from the plan (**not** free
   galley advance for the whole pane).
4. For each `emoji_site`: atlas → if `Some`, `painter.image` in the 1- or
   2-cell span; mono under/instead policy: skip mono for that span when stamp
   succeeds.
5. Existing overlays + kitty graphics (unchanged contracts).

No detection logic, no DirectWrite types, no cache policy in `show`.

## Data flow (one frame)

```
pump() advances grid
show:
  metrics ← font size / "M" probe
  plan    ← plan_paint(grid, metrics)
  for each planned mono glyph (grid-locked positions):
      draw unless replaced by successful emoji stamp
  for each emoji_site:
      match atlas.color_glyph(ch, px):
        Some → texture cache → image(span_rect)
        None → mono only
  overlays(caret, selection, thumb) using same CellMetrics
  kitty placements (existing)
```

**Zoom:** `px` derived from terminal font size and DPR; cache key includes size
so zoom invalidates naturally (same idea as galley `font_bits` today).

## Error handling

| Failure | Behavior |
|---------|----------|
| DirectWrite / font missing | `None` → mono |
| Non-emoji / ZWJ | `None` → mono |
| Cache pressure | LRU (or equivalent) eviction; re-raster on demand |
| Stale grid vs metrics | Frame clamp; no panic |
| Slow first raster | Prefer sync v1 if measured OK; must not block the GUI on network; keep fail-open |

## Phased delivery

Same design; independent reviewable landings:

| Phase | Deliverable | Value |
|-------|-------------|--------|
| **1** | Grid-locked mono via plan for all cells | Caret/CJK/emoji width alignment |
| **2** | `emoji_sites` populated in plan | Detect + tests; still mono paint |
| **3** | DirectWrite atlas + stamps in `show` | Color pickles |

Phase 1 alone meets half the bar (align). Phase 3 meets color. Both required for the stated done bar.

## Testing / verification

| Gate | Proof |
|------|--------|
| Grid lock | Fixture: emoji/CJK + ASCII → glyph columns match model; caret column matches end of input |
| Detect | `🥒` → one `EmojiSite` with `width_cells == 2`; spacer not a second site |
| Atlas fake | Fake returns distinctive RGBA; unit test atlas; optional GUI path |
| Fail-open | `None` still has mono placement |
| Manual | Paste color emoji, type `abc`, caret after letters; emoji multi-color |
| Model untouched | No new PTY width hacks; snapshot/copy still UTF-8 text |

Visual claims need screenshot evidence (build-screenshot / user capture).

## Relationship to existing docs

- `docs/font-fallback.md` — mono fallbacks stay; color is additive stamps.
- `docs/terminal-images.md` — kitty protocol unchanged; emoji is not APC.
- `docs/cursor-rendering.md` — caret still model + Caret gate; alignment fixed by paint matching metrics.
- `docs/warp-feature-candidates.md` — font fallback shovel item; this is follow-on polish.

## Rejected alternatives

| Alternative | Why rejected |
|-------------|--------------|
| **B — stamps only, free galley** | Color without grid lock fails the done bar (caret). |
| **C — bundled emoji atlas only** | Limited set, fat assets, not OS look; keep as emergency if DWrite spike fails. |
| Color via landing-style per-glyph `Color32` | One solid color ≠ multi-color emoji. |
| Fix width by writing spaces/backspaces to PTY | Desyncs ConPTY and alacritty; forbidden. |
| Put DWrite / detect logic in `Session::show` | Shallow module; low locality and testability. |
| Merge emoji into `graphics.rs` | Couples PTY APC protocol with Unicode paint. |

## Key files (expected)

| File | Change |
|------|--------|
| `src/frame.rs` | `plan_paint` / placements + `emoji_sites`; pure tests |
| `src/emoji_raster.rs` (or similar) | DirectWrite + cache + fake; tests |
| `src/terminal.rs` | `show` replays plan + stamps; drop free-flow whole-pane galley for cell text |
| `src/geom.rs` | Reuse `cell_rect` / `span_rect`; no model change |
| `docs/font-fallback.md` | Note color stamps when phase 3 ships |

## Open points for the implementation plan (not design blockers)

- Exact emoji scalar classifier (range tables vs `unicode-segmentation` / existing crates — prefer minimal, tested list/ranges for v1).
- Sync vs deferred raster on first miss (measure after spike).
- Texture cache ownership: per-`Context` vs per-`Session` (prefer shared per Context to reuse across panes).

## Success criteria (done bar)

1. Single-codepoint emoji render **multi-color** when the Windows color font path works.  
2. After paste/type of emoji then ASCII, **caret tracks the grid** (no progressive drift).  
3. CJK paths/names also stay grid-aligned (all-cells paint).  
4. Failure of color path leaves **readable mono**, app stable.  
5. ConPTY/alacritty behavior unchanged (paint-only).  
