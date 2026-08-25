# Snap tiling (edges + corners)

> **SUPERSEDED (2026-08-24):** the 9-zone snap system described below
> (`compose_zone`, `snap_or_tab`, `zone_rect`, `Zone`) was deleted. Tiling is now
> a recursive layout tree. See `docs/tiling-tree.md`. Kept for decision history
> only — do not implement from this file.

## What it does

Snapping tiles a window to half or quarter of its manager's area. You snap with
the leader directional keys (`WASD` for terminals, `Ctrl+WASD` for projects) or by
dragging a window's titlebar to a screen edge/corner.

Keyboard snapping **composes**: each direction pins one axis, so pressing two
perpendicular directions walks you into a corner. There are no dedicated corner
keys — `Left` then `Up` lands you top-left.

## The model

A window's snap is two independent pins:

- horizontal pin: left, right, or none
- vertical pin: top, bottom, or none

Every zone is just a combination — `Left` = (left, none), `Tl` = (left, top),
`Max`/floating = (none, none).

A direction key touches only its own axis:

- if that axis isn't pinned that way yet → pin it
- if it's already pinned that way → release it (toggle off)
- the other axis is left alone

When both pins clear, the window pops back to floating at its pre-snap rect.

That single rule gives the whole behavior:

| Current \ press | ◀ Left | ▶ Right | ▲ Up | ▼ Down |
|---|---|---|---|---|
| Floating / Max | Left | Right | Top | Bottom |
| Left | *float* | Right | Tl | Bl |
| Right | Left | *float* | Tr | Br |
| Top | Tl | Tr | *float* | Bottom |
| Bottom | Bl | Br | Top | *float* |
| Tl | Top | Tr | Left | Bl |
| Tr | Tl | Top | Right | Br |
| Bl | Bottom | Br | Tl | Left |
| Br | Bl | Bottom | Tr | Right |

Reading it in practice:

- **into a corner:** half + perpendicular direction (`Left`+`Up` = Tl)
- **out of a corner:** press a direction you're already pinned to → adjacent half
  (`Tl`+`Up` = Left, `Tl`+`Left` = Top)
- **around corners:** press the opposite direction → neighbour corner
  (`Tl`+`Right` = Tr)
- a half + its own direction still un-snaps to floating (unchanged from before)
- `Max` is *not* reachable by composing; it stays on its own zoom key

## How to use

- Terminal: leader, then `W`/`A`/`S`/`D`. Press a second perpendicular direction
  for a quarter.
- Project: leader, then `Ctrl+W/A/S/D`.
- Mouse: drag the titlebar to an edge for a half, to a corner band for a quarter.
- Snapping onto a zone another window already holds **tabs** the two together
  (same `snap_or_tab` path for edges and corners).

## Gotchas

- `compose_zone` is intentionally pure (no egui) so the state machine is unit
  tested without a context — keep it that way. The table test
  (`compose_zone_matches_full_transition_table`) is the contract; update it if you
  change a transition on purpose.
- Snapping never moves the rect directly; it only sets `w.snap`/`w.prev`. The show
  loop refits snapped windows to `zone_rect` every frame.
- Corners only exist for snapping. `Max` and floating both read as fully un-pinned,
  so a single direction from either always produces a half.

## Key files

- `src/wm.rs` — `compose_zone` (the state machine), `snap_dir` (focused-window
  wrapper), `snap_or_tab` / `set_snap` (commit + tab-on-collision), `zone_rect`
  (layout incl. corners with the tiling split), `Zone` enum.
- `src/keymap.rs` — `Command::TermSnap` / `ProjSnap` and the default `WASD`
  bindings.
