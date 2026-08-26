# Terminal paint cache (the galley atlas)

How a terminal pane avoids re-doing text layout every frame, and every scroll.

## The expensive thing

Before a character reaches the screen, egui has to **lay it out**: pick the
glyph, measure it, place the baseline, bake in the colour. The result is a
`Galley`. Making one is the costly part; drawing one you already have is cheap.

A pane is a grid of cells, each holding a character plus a `GlyphStyle`
(fg, bg, bold, italic, underline, strikethrough). An 80×24 pane is ~1900 cells,
but foreman never makes 1900 galleys — it makes one per **distinct**
`(char, GlyphStyle)` and blits it at every position that needs it. A screen with
400 `e`s in the same style shapes one `e`. That is what `MonoGlyph` is: grid
row/col plus a shared `Arc<Galley>`. Positions are never cached as pixels — the
blit recomputes them from live `CellMetrics` each frame, so moving or resizing a
pane costs nothing extra.

## Two layers

**Layer 1 — the item cache (`MonoPaintCache`).** Holds the finished
`Vec<MonoGlyph>`, background rects, and emoji stamp sites for a pane, under a
`MonoPaintKey`:

| Key field | Bumps when |
| --- | --- |
| `content_gen` | the grid content changed (PTY output, injected note) |
| `off` | `display_offset` — i.e. you scrolled |
| `cols` / `rows` | the pane was resized |
| `font_bits` | zoom |
| `colors` | the live theme's fg/bg/palette changed |

Key matches → re-blit only, zero layout calls. Key misses → rebuild. Selection
and caret are deliberately *not* in the key: they paint as separate overlays, so
dragging a selection never invalidates the text.

**Layer 2 — the galley atlas (`MonoGalleyAtlas`).** A longer-lived
`HashMap<(char, GlyphStyle), Arc<Galley>>` hanging off the same cache.

Layer 2 exists because of one entry in that table: `off`. Scrolling genuinely
changes what's on screen, so it *must* be in the key, so every wheel notch,
page jump and search jump misses layer 1. But the letters don't change when you
scroll — only where they sit. Before the atlas, each miss threw away every
shaped glyph and reshaped the same alphabet from scratch. Now the rebuild looks
them up instead. Measured: **22 glyphs reshaped per wheel notch before, 0 after**
(`real_grid_scroll_repaints_without_reshaping`).

Despite the name it is not a *texture* atlas (egui has one of those internally,
packing glyph rasters into one image). This is a memo table.

## Eviction

The atlas is checked once per rebuild, in `prepare`, and cleared wholesale on
either of two conditions:

- **`font_bits` changed** — zoom invalidates every shaped glyph.
- **more than `MAX_ATLAS_GALLEYS` (8192) entries.**

The size cap matters because `GlyphStyle` holds *resolved* 24-bit colour. Normal
use settles around 2k entries and never trips it, but anything printing many
distinct colours — an image rendered as coloured cells, a gradient, `lolcat`,
each theme you try — mints a near-unique key per cell. Without a cap those live
for the whole session, per terminal.

Two decisions worth not re-litigating:

**Why dump everything instead of an LRU.** Tracking recency costs bookkeeping on
every lookup, in the exact hot path the atlas exists to speed up, and would
likely mean a new dependency. The worst case of a dump — reshape the visible
screen once — is precisely what this code did on *every* scroll before the atlas
existed. The bad case of the cap is the normal case of the old code.

**Why the check is in `prepare` and not in `get_or_insert`.** A big pane can be
~24k cells. Checking per insert would blow the cap several times *inside a single
frame*, each dump discarding galleys that same frame had just shaped. Checked
between rebuilds, a rebuild always finishes with everything it asked for, and
residency peaks at cap + one screen — still bounded.

## Gotchas

- **A cache hit must never call `plan_paint`.** The plan is built only on a miss,
  and the rebuild closure `expect`s it. Note *where* it's built: in a `match` on
  the cache key **before** `mono_paint` is borrowed mutably, because the walk
  reads `self.term` while the rebuild holds `&mut` the cache. So it can't simply
  move inside the closure — and it must not move out of the `match` either, or
  every frame pays the grid walk and the cache quietly stops paying for itself.
- **Emoji sites ride the same key.** They're cached alongside the glyphs so a hit
  still has stamp targets without re-planning, and so a filling emoji atlas
  doesn't bust the mono memo.
- **Emoji glyphs are kept, not dropped.** Their mono blit is suppressed only when
  a colour stamp actually resolved. Dropping them at plan time painted *nothing*
  when the raster failed — tofu is the fail-open.
- **`mono_paint_items_for_test` builds a throwaway atlas,** so back-to-back calls
  through it *do* re-layout. That's intentional; tests that care about reuse pass
  their own atlas.

## How it's tested

`note_layout_call` / `layout_call_count` (`terminal.rs`) are a thread-local
counter incremented inside the real layout closure — thread-local so parallel
`cargo test` workers don't clobber each other. Tests assert on *layout calls*,
which is the actual cost, rather than on wall-clock.

The pins: `mono_paint_cache_hit_does_zero_layouts`,
`mono_paint_scroll_offset_reuses_galleys`,
`mono_paint_cache_miss_on_scroll_reuses_atlas`, `mono_paint_font_change_relayouts`,
`mono_paint_new_glyph_layouts_only_new`, `mono_paint_atlas_dumps_past_cap`,
`mono_paint_atlas_holds_at_the_cap`, and `real_grid_scroll_repaints_without_reshaping`
— the last of which drives a real `Term` through the real VT parser and the real
`plan_paint`, keyed exactly as `show` keys it, so the claim rests on a genuine
`display_offset` change rather than a hand-built plan.

## Key files

- `src/terminal.rs` — `MonoPaintKey`, `MonoPaintCache`, `MonoGalleyAtlas`,
  `MAX_ATLAS_GALLEYS`, `mono_paint_items`, and the blit in `Session::show`
- `src/frame.rs` — `plan_paint`, which walks the grid into a `PaintPlan`
- `src/geom.rs` — `CellMetrics`, the live pixel geometry the blit uses
- `docs/theme-system.md` — why `colors` is in the key (live theme edits)

> **Don't delete `text_rows` expecting dead code.** An earlier design (2026-07-04)
> cached whole-row `StyleRun` spans from `frame::text_rows` behind a single
> `Option<(GalleyKey, Arc<Galley>)>` on `Session`. The per-placement design above
> superseded it. `frame::overlays` from that era is still live, but
> `text_rows` / `StyleRun` no longer have a production caller (the compiler says
> so) — they survive only as a **test oracle**: `plan_paint_clamps_like_text_rows`
> checks the new walk clamps the grid the same way the old one did.
