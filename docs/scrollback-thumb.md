# Scrollback thumb

The thin bar on a terminal pane's right edge. It shows where you are in the
scrollback, and you can grab it.

## What it does

- **Drag the thumb** to scroll. Grabbing it anywhere keeps that grab point, so
  the view doesn't jump when you take hold of the middle.
- **Click the empty track** to jump there — the thumb centres under the cursor,
  and you can keep dragging from that point without letting go.
- Drag past either end of the track and it pins to the live bottom or the oldest
  line rather than running off.

It exists whenever the pane has history, and fades when you leave it alone. It
stays solid while **held** — the pointer is in the track band, a drag is live,
or the offset just moved — then after `THUMB_HOLD` (1s) eases out over
`THUMB_FADE` (0.35s) to a floor that depends on where you are:

- **scrolled back**, or **the pointer is anywhere in this pane** →
  `THUMB_DIM_FLOOR` (30%), faint but present
- **neither** → 0, fully gone

It also stays put while you drag, so it doesn't vanish when the pointer wanders
off the pane mid-drag.

The pointer-in-pane floor is what keeps the thumb findable. The fade originally
counted only the track band as a hold, which meant hovering a terminal showed
*nothing* until you happened to reach for the edge — you cannot grab a control
you have no reason to believe exists. Holding it fully solid on pane hover is the
other extreme: you are hovering a terminal nearly all the time, so it would never
fade and the feature would do nothing. Faint-on-hover, solid-on-approach is the
middle, and `thumb_alpha` takes the floor as a parameter precisely so this stays
the caller's decision.

It is 4px at rest and grows to 8px while the pointer is in the track band or a
drag is live, so it reads as something you can take hold of. The widen is tested
against the 14px band rather than the bar, so it thickens as you approach rather
than only once you are dead on the thin line, and it grows leftward from the same
right edge so it fattens in place instead of shifting.

## How it works

The geometry is pure math in `src/geom.rs`, unit-tested with no GUI:

- `thumb_rect(track, rows, hist, off)` — where the bar sits. 4px wide, with a
  16px minimum height so a deep buffer still leaves something to grab.
- `offset_for_thumb_top(track, rows, hist, y)` — the **exact inverse**: the
  `display_offset` that would put the thumb's top at `y`. Clamped to `0..=hist`.
- `thumb_hit_rect(...)` / `thumb_track_rect(track)` — the grab zone and the
  full-height band, both 14px wide.

`src/terminal.rs` does the input in `show()`, and stores one field on `Session`:
`thumb_drag: Option<f32>`, the pixel gap from pointer to thumb top at grab time.
That is the whole drag state.

Three things about that wiring are load-bearing:

**The bar you see is 4px; the thing you hit is 14px.** A 4px mouse target is not
hittable. Only the hit zone widened — the painted bar is unchanged.

**Engagement is on the press, not on `drag_started`.** egui only fires
`drag_started` once the pointer moves past a threshold, so a plain click on the
track would do nothing until you jiggled the mouse.

**The claim happens before the selection branch.** Local selection otherwise
swallows every primary drag on the pane. `thumb_dragging` is also read *before*
the drag is released, so the frame the button comes up still belongs to the
thumb — otherwise `selection_finished` fires on it and ends a selection the user
never started.

## Gotchas

**The thumb must keep clear of the window's resize band.** This one cost real
time. `wm.rs` puts an invisible `RESIZE_BAND`-wide (6px) resize hit-zone along
every window edge and registers it with `ui.interact` *after* the content, so it
wins the hover. A thumb drawn flush against the edge therefore sits inside that
zone: hovering it hovers the resize handle, the pane's own response stops
reporting `hovered()`, and the thumb **hides exactly when you reach for it** —
and most presses go to the resize handle instead of grabbing it.

So `THUMB_EDGE_INSET` is *derived from* `wm::RESIZE_BAND` rather than being a
hardcoded 6 with a comment, and `thumb_and_its_hit_zone_clear_the_resize_band`
fails if the inset goes back to zero. Widen the resize band and the thumb moves
with it. Anything else you ever put on a pane edge has the same problem.

**The fade must stop asking for frames.** foreman drops to a 100ms repaint
cadence when quiet (`main.rs`), which would render the fade as about three
visible steps — so while a fade is in flight the paint asks for 30ms frames, the
same rate the bell pulse uses in `panel.rs`. `theme::thumb_settled` decides when
to stop. It has to agree with `thumb_alpha`: too eager and the fade freezes
part-way, too lazy and an idle foreman animates forever. A test pins the two
together. This is the app's second animation after the bell — treat any new one
the same way.

**Travel, not track height.** The drag maps over `track_h - thumb_h`, not the
raw track height. Once the 16px floor kicks in on a deep buffer the thumb is
taller than its proportional share, so mapping over the full track would place
the bottom of the range past the end of the track and leave the last stretch of
history unreachable. `thumb_rect` was changed to match; the two are now exact
inverses, which is what `thumb_offset_round_trips_through_thumb_rect` pins.

**An app that owns the mouse still wins.** When mouse reporting is on and Shift
isn't held (`suppress_local`), presses go to the application and the thumb is not
grabbable. That's the same rule the rest of the pane follows, and the usual
mouse-reporting case is an alt-screen TUI, which has no history and therefore no
thumb. If a primary-screen app ever needs to lose this fight, that means a new
precedence rule ahead of `handle_mouse`'s forwarding.

**The right 14px no longer starts a text selection** when the pane has history.
Standard scrollbar trade-off, but it is a behaviour change: a selection that used
to be startable from the extreme right edge now scrolls instead.

## Key files

- `src/geom.rs` — `thumb_rect`, `offset_for_thumb_top`, `thumb_hit_rect`,
  `thumb_track_rect`, `thumb_hot_rect`, `thumb_metrics`, `THUMB_EDGE_INSET`,
  and their tests
- `src/theme.rs` — `thumb_alpha`, `thumb_settled`, `THUMB_HOLD`/`THUMB_FADE`/
  `THUMB_DIM_FLOOR`, and the curve's tests
- `src/wm.rs` — `RESIZE_BAND`, the edge hit-zone the inset has to clear
- `src/terminal.rs` — `Session::thumb_drag`, the claim in `show()`, the paint
- `src/frame.rs` — `overlays()` emits `thumb: Option<Rect>` for the paint
- `docs/terminal-scrollback-search.md` — Ctrl+F, the other way around a buffer
