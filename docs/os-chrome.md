# OS Chrome (auto-hiding title bar)

## What it does

The native Windows title bar is gone (`with_decorations(false)`). Foreman draws
its own replacement bar — title, minimize, maximize/restore, close — but only
while the pointer is at the very top of the app window. The rest of the time the
app is fully chromeless and the content runs edge to edge.

## How to use it

- **Reveal the bar:** move the pointer to the top edge of the app — it opens
  **instantly**. The open zone is `CHROME_REVEAL` (10px from the top): a few
  px past the painted border for an easier hit, short of the project/terminal
  titleband so the in-app ✕ isn't stolen.
- **Hide it:** move the pointer back down past the bar. A short **coyote
  timer** (`CHROME_COYOTE`, ~250ms) holds it open so a brief miss doesn't
  retract it; only after that does it slide up (drawer-style). Re-hovering
  during the coyote window or mid-close cancels the hide and reverse-slides
  the bar back open.
- **Move the window:** drag the revealed bar (hands off to the OS move loop, so
  Aero Snap drag-to-edge still works). Double-click toggles maximize.
- **Resize the window:** grab the outer 5px rim of the window on any edge or
  corner — an invisible handle replaces the native resize border. The cursor
  changes to the usual resize arrows. Disabled while maximized.
- Win+Arrow snapping works as before (it never needed decorations).
- A 7px painted frame (`APP_BORDER`, same color as the revealed bar) marks the
  window edge when windowed — undecorated windows lose the native border.
  Drawn with a layer painter, which paints without registering a widget (so no
  Area-style input blocking); skipped when maximized, like native apps. The
  desktop area is shrunk by the same width when windowed so content sits
  inside the frame, never under it.

## Why

Foreman is its own window manager; the OS title bar was 30px of dead space above
a UI that already has titlebars, tabs, and snapping. Hover-reveal keeps the
min/max/close affordances without paying for them permanently.

## Gotchas

- Open is instant on the thin top border; close is delayed (coyote). Mid-close
  re-hover uses the full bar height (`CHROME_KEEP`) while the slide progress
  is still > 0, so you can catch the retracting bar without finding the 7px
  strip again.
- The bar won't *open* (or re-open mid-close) while a mouse button is down —
  deliberate, so dragging an in-app window to the top edge (snap/maximize
  gesture) never pops the OS bar over the snap zone. While already open,
  clicks on the bar itself still work (min/max/close/drag).
- The revealed bar's interactive area excludes the topmost 5px when windowed:
  that strip belongs to the resize handle (like Chrome's tab strip). Visually the
  bar still paints to the edge.
- The resize rim is an egui `Area` at `Order::Foreground` that claims input, so
  in-app windows flush against the app edge lose their outer 5px to the OS rim.
  That's intended: outermost pixels = OS window, everything inside = foreman.
- `egui::Area` defaults to `movable(true)` — all chrome areas set
  `.movable(false)` explicitly or egui would drag them around.
- **Big egui landmine #1:** an `Area` registers an invisible widget over its
  whole *bounding rect*, and in egui's hit test any widget covering the pointer
  blocks every layer below — regardless of its `Sense`. Putting all four rim
  strips in one Area gave it a full-screen bounding rect and silently ate every
  click/drag in the app. That's why each strip is its own Area.
- **Big egui landmine #2 (the sneaky one):** on an Area's *first frame* egui
  doesn't know the content size, assumes a default (~600×400), and the default
  `constrain(true)` shoves the area's origin up/left so that assumed size fits
  on screen. Combined with absolute-rect content, the *recorded* bounds became
  origin..strip — for the bottom strip that was the entire bottom half of the
  app, and per landmine #1 every window snapped to the bottom (or right) half
  was completely input-dead: no drag, no buttons, no double-click. The rim
  Areas therefore set `.constrain(false)` and `.default_size(strip size)`.
  If chrome interaction ever dies regionally again, dump
  `ctx.memory(|m| m.area_rect(id))` for the chrome Areas first.
- Caption glyphs are painted with line/rect primitives, not font glyphs, so they
  can't fall victim to missing unicode coverage.
- Undecorated windows lose the native drop shadow; on Win11 DWM still rounds the
  corners.

## Key files

- `src/main.rs` — everything: `with_decorations(false)` in `main()`, the
  `CHROME_*` constants, `App::show_os_chrome` (reveal state machine + bar),
  `App::os_resize_rim` (edge resize), `chrome_glyph` (painted caption icons).
