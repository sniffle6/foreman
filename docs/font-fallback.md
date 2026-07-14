# Font fallback (CJK / emoji)

## What it does

Loads Windows system fonts as **egui fallbacks** so CJK characters and emoji
draw as real glyphs instead of empty boxes (tofu). Does not change the
terminal grid or selection math — only which font file supplies the picture.

## Why

egui defaults (Hack + friends) cover Latin well and almost no CJK. Agent
output and paths with Chinese/Japanese/Korean or emoji looked broken even
though the cell model was correct.

## When you see the bug (without this)

- CJK in paths, `ls` / `git`, compiler errors, agent diffs
- Emoji in agent replies or CLIs

Plain ASCII-only panes: you may never notice.

## How it works

At GUI startup (`src/main.rs` → `terminal_font::load_font_definitions`)
registers four matched **Hack v3.003** faces as named terminal families
(regular/bold/italic/bold-italic; see `docs/terminal-font-styles.md`), then
appends system fallbacks:

| Name | Path | Role |
|------|------|------|
| yahei | `C:\Windows\Fonts\msyh.ttc` | CJK (Microsoft YaHei) |
| seguiemj | `C:\Windows\Fonts\seguiemj.ttf` | Emoji shapes (Segoe UI Emoji) |

Missing file → skip. System fonts are **appended** after defaults (primary
mono stays first for non-terminal UI). Each terminal style family starts with
its Hack face, then carries the full Monospace fallback list. egui 0.34's
atlas still does mono outline shapes only — multi-color emoji is additive
(texture stamps on top), not atlas color layers. See **Color stamps** below.

## Gotchas

- ~20MB+ YaHei loaded into RAM at start if present.
- `.ttc` face index is `0` (FontData default); if a machine's YaHei face is wrong, check index.
- Not a cross-platform discovery system — hardcoded Windows paths by design.

## Key files

- `src/terminal_font.rs` — `append_font_fallbacks`, `windows_fallback_font_paths`,
  `load_font_definitions`, four-face terminal families
- `src/main.rs` — `set_fonts` in `run_native` create callback
- `assets/fonts/` — pinned Hack faces + license/provenance

## Color stamps (optional)

### What / why

Multi-color single-codepoint emoji (🥒, 🚀) need bitmaps, not another solid
`Color32` on a `LayoutJob`. DirectWrite rasters color glyphs; Foreman
stamps those RGBA textures over the grid (same idea as kitty graphics
overlays). Mono fallbacks (Segoe UI Emoji via egui) still load for outline
shapes when a stamp misses or the char is not emoji presentation.

### Paint-only

ConPTY and alacritty stay the only model for cells, cursor, and bytes.
No PTY width hacks, no spacer injection, no copy/selection changes — stamps
are paint overlays only.

### Emoji_Presentation

v1 stamps only scalars with **default emoji presentation** (wide color
emoji). Text-default symbols (☁ and friends) stay mono outlines.

### Grid lock / caret

Phase 1 paints **every** cell at `col × cell_w` (from `"M"` metrics), not
free-flow galleys. That keeps the caret and text on the same grid after
emoji/CJK. Design + rules:
`docs/superpowers/specs/2026-07-10-color-emoji-grid-paint-design.md`.

### Perf

Unchanged frame → **no** re-layout of mono glyphs (memoized plan/shapes).
Stamp cache is separate from the mono path so color lookup cost does not
hit every frame's layout gate.

### Key files (stamps)

- `src/frame.rs` — `plan_paint` (mono placements + `emoji_sites`)
- `src/emoji_raster.rs` — DirectWrite / atlas raster + stamp cache
- `src/terminal.rs` — `Session::show` replays plan and draws stamps
