# Toolbar icons (Material Symbols)

Google's Material Symbols Outlined, the 24px default style, one SVG per
glyph. They are the Changes-window toolbar icons (`src/git_history/toolbar.rs`),
rasterized by `src/icons.rs`.

| Field | Value |
|-------|--------|
| Upstream | [google/material-design-icons](https://github.com/google/material-design-icons) |
| Fetched | 2026-10-06 |
| Source URL | `https://fonts.gstatic.com/s/i/short-term/release/materialsymbolsoutlined/<name>/default/24px.svg` |
| License | Apache License 2.0 (see `LICENSE`) |

## Files

| File | Toolbar button |
|------|----------------|
| `refresh.svg` | Refresh |
| `upload.svg` | Push… |
| `add.svg` | Add to VCS |
| `account_tree.svg` | Directories |
| `unfold_more.svg` | Expand All |
| `unfold_less.svg` | Collapse All |
| `chevron_right.svg` | the ">" overflow button |
| `more_horiz.svg` | the ⋯ button the commit row reuses |

Each file is the upstream SVG (viewBox `0 -960 960 960`) with one change:
`fill="#ffffff"` on its `<path>`. `icons.rs` rasterizes a white silhouette
and tints it at paint time, so a path with no fill would render black.

## Refetch

```powershell
curl -fsSL https://fonts.gstatic.com/s/i/short-term/release/materialsymbolsoutlined/refresh/default/24px.svg -o assets/icons/material/refresh.svg
```

then add the `fill` attribute again.
