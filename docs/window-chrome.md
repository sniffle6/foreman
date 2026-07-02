# Window chrome: hover-reveal and the project overflow menu

One reveal rule for BOTH window kinds (desktop projects and in-project
terminals): chrome appears while the pointer is in the window's title band
(`reveal_band` — the top `TITLE_H` strip clipped to the manager area), or
while a header gesture pins it (move drag, rename, tab tear-out, open
overflow menu). Reveal is instant; hide is an alpha fade (`animate_bool` ×
`gamma_multiply`), with click handlers gated on the un-faded state so a
fading-out header never eats a click. The hover test is
`ui.rect_contains_pointer`, which is layer-aware — never replace it with a
raw rect/pointer test or occluded windows reveal through floats.

Projects paint no title bg, and the project *body* paints `terminal::BG`
(not `WIN_BG`) so the reserved band blends seamlessly into the terminals
below it — with chrome grey there, the empty band still read as a header
bar. Revealed project layout is `[icon] [name] [tab chips] … [⋯] [✕]`
(control zone 54 px). The ⋯ popup (open flag in transient egui memory,
never model state) holds: New PS/CMD/SH terminal, New project, Float/Tile,
Minimize, Maximize — each pushing the same `Act` the old header buttons
pushed; it opens below the button and flips above it at the bottom desktop
edge. ✕ stays first-class. Terminals keep their fill and four buttons; a
lone terminal in a project still suppresses its header entirely
(pre-existing rule).

Focus reads from the border alone for projects (`PROJ_BORDER_FOCUS`) —
screenshot-verified legible with three tiled projects.

## Key files

- `src/wm.rs` — `reveal_band` (+ unit test), the `reveal_chrome`/`chrome_t`
  computation, the header paint branch, the overflow menu popup.
- `src/terminal.rs` — `BG`, the shared surface color.
- `docs/superpowers/specs/2026-07-02-quiet-project-chrome-design.md` — the
  approved design and its rejected alternatives (overlay band, two menus,
  whole-project reveal).
