# Terminal font styles (bold / italic)

## What it does

Paints terminal bold, italic, and bold-italic with **real matched font faces**
(Hack Regular/Bold/Italic/BoldItalic), not synthetic shear or double-draw.

## Why

egui 0.34 has no font weight on `FontId` and no synthetic bold.
`TextFormat::italics` is a mesh shear. Without real faces, `git diff` headers,
vim syntax, and man-page emphasis all read flat.

## How to use

Nothing to configure. SGR bold (`ESC[1m`) / italic (`ESC[3m`) select the right
face automatically. Global font **size** / zoom (Ctrl+Scroll, Ctrl+0) still
applies to all four faces.

## Gotchas

- Faces are pinned Hack **v3.003** under `assets/fonts/` (see README + license).
- Metrics probe (`"M"` width/height) uses the **regular** terminal family so
  cell size matches painted glyphs.
- `TextFormat::italics` is forced **false** so the italic face is not sheared
  twice.
- Dim (`ESC[2m`) changes color only; bold still picks the bold face.
- Default Monospace/Proportional families are unchanged for chrome UI.
- User-selectable font family remains a separate backlog item.

## Key files

- `assets/fonts/` — four faces + provenance
- `src/terminal_font.rs` — registration, `font_id(size, bold, italic)`
- `src/terminal.rs` — `GlyphStyle.bold/italic`, paint layout
- `src/main.rs` — `set_fonts` at startup
