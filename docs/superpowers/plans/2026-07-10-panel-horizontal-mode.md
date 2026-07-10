# Panel Horizontal Mode Implementation Plan

> **For agentic workers:** Execute with the **hybrid-plan-execution** protocol.
> The full self-contained protocol text lives in
> `docs/superpowers/plans/2026-07-09-task-manager-panel.md` § "Execution
> Protocol" — same rules apply here (risk tiers M/S/C, verification vs review,
> per-task commits). Steps use checkbox syntax for tracking.

**Goal:** When the task-manager panel's content rect is wider than tall
(bottom/top-docked in the tree), its content flows left-to-right instead of
top-to-bottom — and collapse/expand keeps working in that orientation.

**Mockup (approved):**
`docs/superpowers/specs/2026-07-10-task-manager-panel-horizontal-mockup.html`
— open it in a browser; the three toggles are the three states to build.
Companion vertical mockup: `2026-07-09-task-manager-panel-mockup.html`.

**The rule:** in `PanelView::show`, branch on `rect.width() > rect.height()`
of the content rect it already receives each frame. No new state, no
persistence — move the leaf to a tall slot and it flips back. Tree moves are
atomic, so a discrete per-frame check needs no hysteresis.

## Current state (what exists, verified 2026-07-10, commit ee37a47)

- `src/panel.rs` — `PanelView::show` (vertical rows), `paint_row` (one 22px
  row: icon, truncated title, hover min/close, `min`/`tab` markers),
  `paint_rail` (vertical 36px icon rail), `PANEL_W`/`RAIL_W` consts.
- `src/wm.rs` — `apply_panel_ratio` sizes the panel leaf **width** every
  frame while collapsed (rail pin) and on toggle/ensure; `panel_prefs`,
  `toggle_panel`, `sync_panel_width_from_layout` (persists divider drags into
  `expanded_width` when expanded); resize handles skip the collapsed panel
  (`pinned`); header special-cases the panel (collapse-only control at
  ~wm.rs:3260 `is_panel` branch; title suppressed when collapsed).
- `src/layout.rs` — `set_leaf_width(id, target_px, area, gap)`: pins a leaf's
  width via its nearest **H-axis** divider; the pinned leaf may go below
  `MIN_RATIO` (floor 2px), siblings clamp at `MIN_RATIO`. Shares
  `find_interior_split` with `resize_edge`.
- `TargetPath.ptab` disambiguates tabbed projects — keep populating it from
  any new row-building code.

## Design decisions (from the approved mockup — don't relitigate)

1. **Columns mode** (horizontal, body ≥ 2 rows tall): one group per project,
   fixed ~200px wide, project row on top + its tab rows below, vertical
   hairline between groups, horizontal scroll. `paint_row` is reused
   verbatim — only cursor advance changes (y within group, x between groups)
   and the truncation budget comes from group width, not panel width.
2. **Strip mode** (horizontal, body < ~48px ≈ project row + one tab row):
   one line of inline chips — project chip then its terminal chips, hairline
   divider between projects. Click = surface (`self.click`). No hover
   min/close in strip mode; management means expanding first.
3. **Rail, horizontal** (collapsed): a 36px-tall strip, project icons
   left-to-right, expand glyph at the far right INSIDE the strip — no
   separate header band (36px can't fit band + body).
4. **Collapse glyph orients to the shrink axis:** `»`/`«` when right-docked
   (today's behavior), `⌄`/`⌃` when bottom-docked. Top/left docks mirror.
5. **Scroll axis flips with the flow:** wheels have no x axis —
   `smooth_scroll_delta.y` drives the x scroll offset in horizontal mode.

## Global constraints

- Windows/PowerShell, GNU toolchain. Kill the app before building:
  `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue` (else
  `Access is denied (os error 5)`). The kill hook fires for Bash only, NOT
  the PowerShell tool.
- Tests: plain `cargo test <substring>` — this is a bin-only crate;
  `cargo test --lib` fails with "no library targets".
- Colors from `src/theme.rs` consts only; no new hex literals.
- All row interactions stay on the deferred pattern: record into
  `PanelView::click`/`hover_act`, drained by `drain_panel_acts`. Never
  mutate the tree mid-draw.
- **egui hover trap (cost us the min/close buttons once):** child widgets
  registered on top of a row steal `hovered()` from it. Gate button
  visibility/registration on `resp.hovered() || resp.contains_pointer()`,
  never on `hovered()` alone. See `paint_row`'s comment.
- `cargo fmt` currently wants to reformat `src/chat_view.rs` and
  `src/dirpicker.rs` (pre-existing drift, unrelated). Don't sweep them into
  feature commits — `git checkout --` them or land a separate `style:` commit
  first.
- Commit per task: `type(scope): subject` + trailer
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`. Stage by name.

## Tasks

### Task 1 — layout: generalize `set_leaf_width` → `set_leaf_extent` (S)

- [ ] `LayoutTree::set_leaf_extent(id, axis: SplitDir, target_px, area, gap)`:
  identical to today's `set_leaf_width` but the probed axis is a parameter
  (`find_interior_split` already takes `axis`; today it's hardcoded
  `SplitDir::H`). Keep `set_leaf_width` as a one-line wrapper or update the
  two wm callers — implementer's choice, but no behavior change for H.
- [ ] Tests mirroring the existing three `set_leaf_width_*` tests for the V
  axis (pin a bottom leaf's height below MIN_RATIO; nested-in-H-row case;
  sole-leaf no-op + sibling clamp).
- Verify: `cargo test layout::` all green.

### Task 2 — wm: orientation-aware panel sizing + collapse (S)

- [ ] Detect the panel's dock axis each frame: the constrained axis is the
  one `set_leaf_extent` can actually pin — try H first (today's behavior),
  fall back to V. Recommended shape: `apply_panel_ratio` computes
  `target_px` as today, then calls `set_leaf_extent(pid, SplitDir::H, ...)`
  and, if that returns false, `set_leaf_extent(pid, SplitDir::V, ...)`.
  (A right-docked panel has an H divider; a bottom-docked one may have only
  a V divider. A panel with both dividers stays width-pinned — fine.)
- [ ] `sync_panel_width_from_layout`: when the panel rect is wider than tall
  and only a V divider exists, persist `rect.height()` into
  `expanded_width` instead (the setting is "expanded extent along the
  dock axis"; keep the `panel_width` settings key — do NOT rename a
  persisted field).
- [ ] Collapsed horizontal: the whole window is a 36px strip — suppress the
  header band for the collapsed panel when horizontal (the `is_panel`
  header branch, ~wm.rs:3260) and let `PanelView::show` receive the full
  window rect; the expand toggle moves INSIDE the rail (Task 3). The
  collapse control in the EXPANDED horizontal header uses `⌄` (or `⌃` when
  top-docked; `scr` vs desktop area tells you which side).
- [ ] Resize-handle `pinned` skip already keys off `v.collapsed` — unchanged.
- [ ] Tests: pin-height test through `apply_panel_ratio` with a bottom-docked
  panel (build the tree by hand: `tree.insert_root(other, Dir::Right)` then
  `tree.insert_root(pid, Dir::Down)`), assert leaf height ≈ RAIL_W when
  collapsed and ≈ expanded_width when expanded.
- Verify: `cargo test wm::` green.

### Task 3 — panel: horizontal flows in `PanelView::show` (C — visual)

- [ ] Branch at the top of `show`: `let horizontal = rect.width() > rect.height();`
  Vertical path stays byte-identical.
- [ ] **Columns:** group cursor advances x by `GROUP_W` (~200.0) + hairline;
  within a group, rows advance y from the group top, clipped to the group
  and to `rect`. Reuse `paint_row` with the group rect for truncation.
  Rows that overflow the group height are simply clipped (scroll is
  horizontal only).
- [ ] **Strip:** when `rect.height()` < header-less body of ~48px, paint
  chips: project chip (folder icon + name, focused wash + hairline stripe
  when focused) then terminal chips (icon + short title), `sdiv` hairline
  between projects. Click = `self.click = Some(path)`. Populate `ptab`.
- [ ] **Rail:** icons advance x; expand glyph (`⌃`/`⌄` per dock side) drawn
  at `rect.max.x - 14` inside the strip, records `toggle_collapse`.
- [ ] **Scroll:** in horizontal mode, `self.scroll` offsets x; feed it from
  `smooth_scroll_delta.y` (and `.x` too — some trackpads have it), clamp to
  content width.
- [ ] Hover trap rule applies to any new interactive element (see Global
  constraints).
- Verify (evidence, not eyeballing): temp startup harness in `main.rs`
  (`if !self.started` block): spawn 2–3 projects, then move the panel to the
  bottom — `self.desktop` tree ops: remove panel leaf + `insert_root(pid,
  Dir::Down)` — build, screenshot all three states (columns / strip via a
  short panel / rail), Read the PNGs, REVERT the harness. Screenshot script:
  `docs/HANDOFF.md` § 3.

### Task 4 — docs + wrap-up (M)

- [ ] Update `docs/task-manager-panel.md`: horizontal mode rule, the three
  states, `set_leaf_extent` in Key files, glyph orientation note.
- [ ] Run `foreman-reviewer` on the diff (wm/layout/panel invariants).
- [ ] Full `cargo test`; commit per convention.

## Open questions (ask the user only if they block you)

- Strip-mode chip labels: mockup shows short ids ("t1", "t3"). If real tab
  titles are long, truncate chips at ~90px. Don't add tooltips-on-chips in
  v1 unless trivial.
- Top-docked and left-docked panels fall out of the same code paths (axis +
  glyph mirroring); left-dock needs no new work at all. Don't build special
  cases for them — just don't break them.
