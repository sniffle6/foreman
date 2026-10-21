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

## Tab overflow: the "+N" chip

Dispatch stacks every worker after the first onto one window
(`docs/tiling-tree.md`), so a plan run often has more tabs than its header
can fit. Chips never shrink. Instead:

- **The active tab always has a chip.** Chips pack from tab 0 while the
  active tab fits. When it doesn't, the visible run slides so the active
  chip is the last one that fits. There is no scroll state: the run is a pure
  function of chip widths and the active index (`visible_tabs`).
- **"+N" stands in for the rest.** When any tab is hidden, a 30pt chip at the
  end of the strip (before a project's `+`) shows how many. Its room comes off
  the fence before packing, so it never touches the controls. With nothing
  hidden the header is exactly what it was before overflow existed.
- **It carries the hidden tabs' attention.** It pulses with the bell colour
  while a hidden tab's Bell rings (the chips' pulse), and its label turns the
  needs-you colour while a hidden agent needs you (with agent-state badges
  on). A worker waiting on you shows without opening the Sessions panel.
- **Click it for the list.** One row per hidden tab: icon, title, and the
  Sessions panel's own badge (`panel::paint_attention_badge`). A row click
  is a chip click (`Act::SetTab`). Escape, a press outside, or "+N" again
  closes it.
- Every tab stack gets it: terminals, projects, viewers.

Gotchas:

- Past the fit, clicking a visible chip other than the last one shifts the
  run left by one, because the clicked tab becomes the last chip. That is the
  price of having no scroll state.
- While the list is open, no terminal in that manager reads keys and the
  leader is dormant, as with the layout menu. Opening it focuses its window
  (like any header click); otherwise a focused terminal in another project
  would keep the keys, Escape included. The list closes on any frame its
  window draws no "+N" (minimized, behind another tab, widened until all
  fit), so it can never hold the keyboard while nobody can see it.
- Its egui ids hang off the manager's `base` and the window id
  (`(id, "tabovf")`, `(id, "tabovf-menu")`). Every manager numbers windows
  from 1, so an id without `base` would merge the desk's list with a
  Project's.

## Key files

- `src/wm.rs` — the `bare` rule, `header_layout` (the pure geometry module:
  chip packing, the control-zone fence, the `+` clamp, the "+N" reserve —
  contract-tested), `visible_tabs` (which tabs get a chip), `show_tab_overflow`
  (the "+N" list), `paint_header_label` (shared clipped text and overflow
  fade), the header paint branch, the `+`/`⋯` menu wiring.
- `src/panel.rs` — `paint_attention_badge`, the right-edge badge the
  Sessions panel and the "+N" list share.
- `src/launcher.rs` — shared ordered launcher entries, with live keymap hints.
- `src/hover_menu.rs` — grouped hover menu painting and open/close rules.
- `src/theme.rs` — every color token in one place (`bg` the shared surface
  color, the border/focus ladder, selection whites, app chrome greys, chat and
  ANSI palettes). Runtime `Theme` + the `seed_live`/`live` seam — see
  `docs/theme-system.md`.
- `docs/superpowers/specs/2026-07-02-quiet-project-chrome-design.md` — the
  approved design and its rejected alternatives (overlay band, two menus,
  whole-project reveal).
