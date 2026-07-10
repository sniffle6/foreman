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

At GUI startup (`src/main.rs`), `load_font_definitions` reads:

| Name | Path | Role |
|------|------|------|
| yahei | `C:\Windows\Fonts\msyh.ttc` | CJK (Microsoft YaHei) |
| seguiemj | `C:\Windows\Fonts\seguiemj.ttf` | Emoji shapes (Segoe UI Emoji) |

Missing file → skip. Fonts are **appended** after defaults (primary mono
stays first). Color emoji layers are not supported in egui 0.34 — shapes only.

## Gotchas

- ~20MB+ YaHei loaded into RAM at start if present.
- `.ttc` face index is `0` (FontData default); if a machine's YaHei face is wrong, check index.
- Not a cross-platform discovery system — hardcoded Windows paths by design.

## Key files

- `src/main.rs` — `append_font_fallbacks`, `windows_fallback_font_paths`,
  `load_font_definitions`, `set_fonts` in `run_native` create callback
