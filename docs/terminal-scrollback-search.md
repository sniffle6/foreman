# Scrollback search (Ctrl+F)

## What it does

Host-side regex search over a pane's full scrollback (not only the viewport).
Opens with Ctrl/Cmd+F, highlights matches, jumps with Enter/n/N, closes with Esc.

## Why

Finding earlier agent output or build logs is the most common scrollback need.
Unlimited UI-thread scans would freeze the compositor on large history, so work
is **bounded per frame**.

## How to use

| Key | Editing | Navigating |
|-----|---------|------------|
| Ctrl+F | open / focus | return to editing |
| type | edit regex query (live search) | (ignored — use Ctrl+F first) |
| Enter | confirm → Navigating | next match |
| n / Shift+Enter | — | next |
| N / Shift+n | — | previous |
| Esc | close (viewport stays at match) | close |

Smart-case: lowercase query is case-insensitive; any uppercase letter makes it
case-sensitive (alacritty `RegexSearch` behavior). Invalid regex is nonfatal
(shown in the bar).

## Bounds

- **One model tick per UI frame** (keys queue nav; paint does not double-tick)
- Shared **≤ 1000 lines** + ~4 ms wall budget across seek + count + visible
  work — never `search_next(..., None)` on the UI thread
- Query changes rebuild **immediately** (not subject to output quiescence)
- Content/resize changes clear stale highlights, then rescan only after an
  80 ms quiescence that slides **only when a new generation is observed**
- Full-buffer **count** walks from the oldest history line, chunked
- Next/prev **seeks** wrap **exactly once**, reject non-progressing alacritty
  hits, and stop after a full traversal
- Match count capped at **100_000**; `+` only after proving one hit beyond the cap
- Focused ordinal is reconciled as the count walk passes the focused span
- Empty query / invalidation clears visible highlights immediately
- Only focused + visible matches are retained (not every hit of `.`)
- Repaint is requested only while scan/seek/quiescence still has work

## Gotchas

- While open: focused `TextEdit` owns keyboard (blocks WM leader chords),
  caret hidden, app mouse reporting suspended, wheel = local history scroll
- Search bar hit area is excluded from terminal selection and right-click paste;
  local selection still works outside the bar
- Opening search releases any in-flight app mouse buttons
- Hidden/inactive tabs / OS focus loss cancel mouse captures; search field
  surrenders focus but keeps query/results
- Leader `Ctrl+B, F` is blocked while the search field is focused (raw
  Ctrl+F only opens search); while **Editing**, `n`/`N` are text not nav
- Query truncation is UTF-8 safe (emoji cannot panic the GUI)
- Resize drops/rebuilds match coordinates safely; optional "keep scroll on
  resize" viewport policy is a **separate** follow-up (not this feature)

## Key files

- `src/search.rs` — model, bounded scan, tests
- `src/terminal.rs` — `handle_search_keys`, overlay paint, bar
- `src/input.rs` — `InputOutcome::open_search`, Ctrl+F frame drain
- `src/theme.rs` — `SEARCH_MATCH` / `SEARCH_CURRENT` / bar tokens
