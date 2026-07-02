# Window chrome: always-on quiet headers and the project menus

Every non-bare window shows its chrome unconditionally — there is no
hover-reveal and no fade. **Projects**: `[icon] [name] [+] [tab chips] …
[⋯] [✕]`. **Terminals**: `[icon] [title] [tab chips] … [grid/min/max/✕]`.
The single exception is the pre-existing **bare** rule (`src/wm.rs` "bare"
path): a lone, tiled, single-tab non-project window draws no chrome at all —
the project frame is its only frame.

Headers sit on a **reserved band** at BOTH levels: content starts below
`TITLE_H`, so a terminal's PTY grid gives up a row rather than hiding one
under an overlay. No title bg is painted anywhere; instead every window body
paints `terminal::BG` (the terminal surface color), so bands blend
seamlessly into the content below them — chrome grey there reads as a
header bar even with no fill. Focus reads from the border
(`PROJ_BORDER_FOCUS` / `BORDER_FOCUS`) plus text brightness.

Two hover-opened menus (`hover_menu` — opens on anchor hover, no click;
closes on item click, Escape, or pointer leaving anchor+panel; open flag in
transient egui memory, never model state; flips above the anchor at the
bottom desktop edge):

- `+` (after the name): New project, New PS/CMD/SH terminal.
- `⋯` (right, next to ✕): Float/Tile, Minimize, Maximize.

Each item pushes the same `Act` the old header buttons pushed. ✕ stays
first-class. The `+` anchor is clamped against the reserved control zone
(`ctl_w`) so packed tab chips or a long project name can never push it into
overlap with `⋯` (which would open both hover menus at once).

## Key files

- `src/wm.rs` — the `bare` rule, `header_layout` (the pure geometry module:
  chip packing, the control-zone fence, the `+` clamp — contract-tested),
  the header paint branch that consumes it, `hover_menu`, the `+`/`⋯` menu
  wiring.
- `src/theme.rs` — every color token in one place (`BG` the shared surface
  color, the border/focus ladder, selection whites, app chrome greys, chat and
  ANSI palettes). Static consts by design — no runtime theme system until a
  second theme exists.
- `docs/superpowers/specs/2026-07-02-quiet-project-chrome-design.md` — the
  approved design and its rejected alternatives (overlay band, two menus,
  whole-project reveal).
