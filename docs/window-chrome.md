# Window chrome: hover-reveal and the project overflow menu

Two reveal behaviors, one mechanism. **Projects**: quiet chrome, always
visible — `[icon] [name] [+] [tab chips] … [⋯] [✕]` floating directly on the
window surface with no title bg fill. **Terminals**: chrome appears while the
pointer is in the window's title band (`reveal_band` — the top `TITLE_H`
strip clipped to the manager area), or while a header gesture pins it (move
drag, rename, tab tear-out); hide is an alpha fade (`animate_bool` ×
`gamma_multiply`) with click handlers gated on the un-faded state. The hover
test is `ui.rect_contains_pointer`, which is layer-aware — never replace it
with a raw rect/pointer test or occluded windows reveal through floats.

The project *body* paints `terminal::BG` (not `WIN_BG`) so the header band
blends seamlessly into the terminals below it — with chrome grey there, the
band reads as a bar even with no fill.

Two hover-opened menus (`hover_menu` — opens on anchor hover, no click;
closes on item click, Escape, or pointer leaving anchor+panel; open flag in
transient egui memory, never model state; flips above the anchor at the
bottom desktop edge):

- `+` (after the name): New project, New PS/CMD/SH terminal.
- `⋯` (right, next to ✕): Float/Tile, Minimize, Maximize.

Each item pushes the same `Act` the old header buttons pushed. ✕ stays
first-class. Terminals keep their fill and four buttons; a lone terminal in
a project still suppresses its header entirely (pre-existing rule).

Focus reads from the border alone for projects (`PROJ_BORDER_FOCUS`) —
screenshot-verified legible with three tiled projects.

## Key files

- `src/wm.rs` — `reveal_band` (+ unit test), the `reveal_chrome`/`chrome_t`
  computation, the header paint branch, the overflow menu popup.
- `src/terminal.rs` — `BG`, the shared surface color.
- `docs/superpowers/specs/2026-07-02-quiet-project-chrome-design.md` — the
  approved design and its rejected alternatives (overlay band, two menus,
  whole-project reveal).
