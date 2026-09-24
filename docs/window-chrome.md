# Window chrome: always-on quiet headers and the project menus

Every non-bare window shows its chrome unconditionally — there is no
hover-reveal animation. **Projects**: `[icon] [name] [+] [tab chips] …
[⋯] [✕]`. **Terminals**: `[icon] [title] [tab chips] … [grid/min/max/✕]`.
The single exception is the pre-existing **bare** rule (`src/wm.rs` "bare"
path): a lone, tiled, single-tab non-project window draws no chrome at all —
the project frame is its only frame.

Headers sit on a **reserved band** at BOTH levels: content starts below
`TITLE_H`, so a terminal's PTY grid gives up a row rather than hiding one
under an overlay. No title bg is painted anywhere; instead every window body
paints `th.bg` (the shared terminal surface color), so bands blend
seamlessly into the content below them — chrome grey there reads as a
header bar even with no fill. Focus reads from the border
(`th.proj_border_focus` / `th.border_focus`) plus text brightness.

Two hover-opened menus (`src/hover_menu.rs` — opens on anchor hover; closes
on item click, anchor click, Escape, or pointer leaving anchor+panel; open
flag in transient egui memory, never model state; flips above the anchor at
the bottom desktop edge). Both use the live theme's main surface color and
selection fill:

- `+` (after the name): click creates a default-shell terminal; hover opens
  the shared launcher with Agents, Shells, Project tools, and New project.
  Agent rows inject the command into a normal shell; unavailable agents show
  an error toast. Open tools carry a dot and their live keymap chord.
- `⋯` (right, next to ✕): Float/Tile, Minimize, Maximize.

Selections defer their action until after painting. ✕ stays first-class.
The `+` anchor is clamped against the reserved control zone
(`ctl_w`) so packed tab chips or a long project name can never push it into
overlap with `⋯` (which would open both hover menus at once).

Single-tab titles are clipped to a text-only slot before the control zone (and
before the `+` on projects). Overflow fades into the surface color with the same
paint helper as tab labels; fitting titles remain fully visible without a fade.
When no text space remains, the label paints nothing. Icons and control hit
regions are unchanged. The collapsed Sessions rail still omits its label.

## Key files

- `src/wm.rs` — the `bare` rule, `header_layout` (the pure geometry module:
  chip packing, the control-zone fence, the `+` clamp — contract-tested),
  `paint_header_label` (shared clipped text and overflow fade), the header paint branch, the `+`/`⋯` menu
  wiring.
- `src/launcher.rs` — shared ordered launcher entries, with live keymap hints.
- `src/hover_menu.rs` — grouped hover menu painting and open/close rules.
- `src/theme.rs` — every color token in one place (`bg` the shared surface
  color, the border/focus ladder, selection whites, app chrome greys, chat and
  ANSI palettes). Runtime `Theme` + the `seed_live`/`live` seam — see
  `docs/theme-system.md`.
- `docs/superpowers/specs/2026-07-02-quiet-project-chrome-design.md` — the
  approved design and its rejected alternatives (overlay band, two menus,
  whole-project reveal).
