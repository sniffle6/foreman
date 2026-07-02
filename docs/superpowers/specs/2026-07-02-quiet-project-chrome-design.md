# Quiet Project Chrome — Design

**Status:** awaiting user review. Three judgment calls were made while the user
was away (marked ⚖ below); each is a one-line change if overridden.

## Goal

Project headers currently always show: name, tab chips, `PS · CMD · SH` shell
chips, `+` new-project, float, min, max, close — eight interactive targets per
project, always painted. Make project chrome as quiet as terminal chrome:
invisible until you reach for it, and calm even when revealed.

## Current state (verified against `src/wm.rs` @ 53786d7)

- `reveal_chrome = is_project || is_renaming || dr.dragged() || tab_dragging ||
  pointer-on-window` (`src/wm.rs:2588`) — terminals hover-reveal; projects are
  hard-wired always-on.
- Project headers **reserve** their strip: project `content_rect` starts below
  `TITLE_H`; terminal headers paint **over** full-bleed content.
- Header bg: `PROJ_TITLE_BG` / `PROJ_TITLE_BG_FOCUS`; focus also shows via
  `PROJ_BORDER_FOCUS`.

## Design (Approach A — recommended and specced)

### 1. Reveal rule ⚖

Projects drop the `is_project` escape hatch and get their own trigger: pointer
inside the **title band** (`title_rect`, the top `TITLE_H` strip), with the
existing pins (move drag, rename, tab tear-out) plus a new pin: **overflow menu
open**. No dwell — reveal is immediate (the OS bar's dwell exists to guard
accidental brushes at the screen edge; a project band has no such hazard).

⚖ *User's message said "hovered over that project" (whole-project reveal, the
terminal rule). Rejected here because the pointer usually parks in the project
you're working in, so its header would be visible almost constantly — the
single-project case that motivated this stays busy. The predicate lives in one
small function; flipping to whole-project reveal is a one-line override.*

### 2. Band stays reserved; bg goes transparent

The title strip keeps its reserved `TITLE_H` (project `content_rect` is
unchanged). Hidden state paints **nothing** — the band shows the window bg,
reading as margin. Revealed state paints **no title bg fill** either (name,
chips, buttons float on the window bg).

Rejected alternative (Approach C): overlay-style header like terminals,
reclaiming `TITLE_H`. The band would paint over — and steal input from — the
nested terminals' own headers at the project's top edge. Not worth the 24 px.

### 3. Revealed layout: one overflow menu ⚖

```
[name]  [tab chips…]                              [⋯] [✕]
```

- **Name**: unchanged — drag handle (tear-out/move), double-click renames.
- **Tab chips**: unchanged, only when the project stack has >1 tab.
- **`⋯` overflow menu** (egui `Area` popup anchored under the button), flat
  list, no submenus: `New PS terminal`, `New CMD terminal`, `New SH terminal`,
  `New project`, `Float/Tile`, `Minimize`, `Maximize`. Menu-open pins the
  header revealed.
- **`✕`**: stays first-class (never buried behind a click).

⚖ *User proposed two menus: `⋯` for window buttons plus a `∨` chevron holding
name + terminal buttons. Rejected: two unlabeled menus a few pixels apart is
double mystery-meat, and the name must stay visible anyway — it is the drag
handle and the only "which project is this" signal. One menu, name as text.*

### 4. Fade animation

`ctx.animate_bool(header_id, revealed)` → alpha 0..1; all header paints go
through `Color32::gamma_multiply(t)`. Same feel as the OS chrome slide, minus
the slide (band is reserved; sliding would be motion for its own sake).
Interact rects for header affordances only registered while `t > 0.5` so a
fading-out header can't swallow a click meant for content below the band —
N/A for reserved band, but guards future overlay conversion.

### 5. Focus signal ⚖

With `PROJ_TITLE_BG_FOCUS` gone, focus reads from `PROJ_BORDER_FOCUS` alone.
⚖ Judgment: ship it and screenshot-check legibility with 3+ projects; if weak,
bump focused border width `BORDER_W`→2 px for projects rather than reintroduce
a bg fill.

### 6. Out of scope

Terminal headers (already quiet), the OS chrome bar, minimized taskbar,
drop-hint painting during drags (separate painter, unaffected by reveal).

## Testing

- Unit (pure, wm tests): reveal predicate — band hover reveals; content hover
  does not; drag/rename/menu-open pin; extract predicate so it's testable
  without egui.
- Evidence (per foreman-validation-and-qa): screenshots — hidden state,
  hover-revealed state, overflow menu open, focus legibility with 3 projects.
- Existing 373 tests stay green.

## Key files

- `src/wm.rs` — reveal predicate (`:2588`), header paint branch
  (`:2641-3127`), new overflow-menu popup. Menu-open state lives in transient
  egui memory (`ctx.data`), not on `Win` — it is per-frame UI state, not
  model state, and must not leak into wire/model serialization.
