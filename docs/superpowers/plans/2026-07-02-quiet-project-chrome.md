# Quiet Project Chrome Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Hover-revealed, transparent project headers sharing one top-band
reveal rule with terminal headers, with all project header buttons except ✕
collapsed into a `⋯` overflow menu.

**Architecture:** All work is in `src/wm.rs`'s per-window render loop. A pure
`reveal_band` geometry fn feeds the layer-aware `ui.rect_contains_pointer`
check; both window kinds use it. Project headers lose their bg fill and their
six button affordances in favor of an `egui::Area` popup menu whose open flag
lives in transient egui memory. A `ctx.animate_bool` alpha fades the whole
header branch.

**Tech Stack:** Rust, egui 0.34 (`eframe`), existing wm test module (pure,
no GUI).

**Spec:** `docs/superpowers/specs/2026-07-02-quiet-project-chrome-design.md`

## Global Constraints

- Branch: `feat/quiet-project-chrome`. Commit per task, stage files BY NAME
  (never `git add -A`).
- Commit trailer (exact): `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Build on Windows/GNU. From Bash, a PreToolUse hook kills a running foreman
  for `cargo build/run/test`; from PowerShell run
  `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue` first or
  linking fails with `Access is denied (os error 5)`.
- `cargo test` (373 green at branch point) must stay green after every task.
- egui 0.34: text measuring goes through `ui.painter().layout_no_wrap(...)`
  (NOT `ui.fonts(|f| ...)`, which needs `&mut`). Widget Ids must derive from
  `base` (the manager-unique Id) to avoid nested-Project collisions.
- Terminal header *styling* is untouched: bg fill, tab chips, buttons all stay.
  Only the terminal reveal *trigger* changes (Task 1).
- No `src/control.rs` / wire-format changes anywhere in this plan.
- The reveal hover check MUST use `ui.rect_contains_pointer` (layer-aware:
  respects floats occluding tiles). Never test a raw pointer pos against the
  band rect — that reveals headers under floating windows.

---

### Task 1: Shared top-band reveal rule

**Files:**
- Modify: `src/wm.rs:2586-2592` (the `reveal_chrome` computation)
- Modify: `src/wm.rs` module-level fns (near `fn clamp`, ~line 540) — add `reveal_band`
- Test: `src/wm.rs` tests module (append near `closing_a_tiled_window_collapses_its_slot`)

**Interfaces:**
- Produces: `fn reveal_band(scr: egui::Rect, area: egui::Rect) -> egui::Rect`
  (module-level free fn, pub(crate) not needed — same module). Task 2 reads
  the menu-open flag id `base.with((id, "ovfmenu_open"))` introduced here.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src/wm.rs`:

```rust
    #[test]
    fn reveal_band_is_the_title_strip_clipped_to_area() {
        let scr = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(200.0, 100.0));
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(500.0, 500.0));
        let band = reveal_band(scr, area);
        assert_eq!(band.min, egui::pos2(10.0, 10.0));
        assert_eq!(band.width(), 200.0);
        assert_eq!(band.height(), TITLE_H);

        // A window hanging off the top of the area gets a clipped band.
        let scr2 = egui::Rect::from_min_size(egui::pos2(10.0, -10.0), egui::vec2(200.0, 100.0));
        let band2 = reveal_band(scr2, area);
        assert_eq!(band2.min.y, 0.0);
        assert_eq!(band2.height(), TITLE_H - 10.0);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test reveal_band 2>&1 | tail -5`
Expected: compile error — `cannot find function reveal_band`.

- [ ] **Step 3: Implement `reveal_band`**

Add as a module-level free fn (next to the other free fns like `clamp`):

```rust
/// The hover strip that reveals a window's chrome: the title band, clipped to
/// the manager's visible area. Shared by projects and terminals — one reveal
/// rule at both levels (spec 2026-07-02). The caller must test the pointer
/// with `ui.rect_contains_pointer(reveal_band(..))`, which is layer-aware;
/// a raw `band.contains(pointer)` would reveal chrome on windows occluded by
/// a floating window above them.
fn reveal_band(scr: egui::Rect, area: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_size(scr.min, egui::vec2(scr.width(), TITLE_H)).intersect(area)
}
```

- [ ] **Step 4: Swap the call site — both window kinds**

At `src/wm.rs:2586-2592`, replace:

```rust
            let tab_dragging = (0..self.windows[i].tabs.len())
                .any(|ti| ui.ctx().is_being_dragged(base.with((id, "tab", ti))));
            let reveal_chrome = is_project
                || is_renaming
                || dr.dragged()
                || tab_dragging
                || ui.rect_contains_pointer(scr.intersect(area));
```

with:

```rust
            let tab_dragging = (0..self.windows[i].tabs.len())
                .any(|ti| ui.ctx().is_being_dragged(base.with((id, "tab", ti))));
            // Reveal pins: header gestures that must keep a fading header
            // alive mid-flight, plus the project overflow menu while open.
            let menu_open = ui
                .ctx()
                .data(|d| d.get_temp::<bool>(base.with((id, "ovfmenu_open"))))
                .unwrap_or(false);
            let pinned = is_renaming || dr.dragged() || tab_dragging || menu_open;
            let reveal_chrome = pinned || ui.rect_contains_pointer(reveal_band(scr, area));
```

Also update the comment block directly above (`:2578-2585`): it starts with
"Non-project chrome is hover-revealed" — reword to say ALL chrome is
band-revealed, e.g.:

```rust
            // ALL window chrome (projects and terminals) is hover-revealed by
            // ONE rule: the pointer must be in the title band, not merely on
            // the window (spec 2026-07-02). Content owns the full window rect
            // so grids never resize on hover; the header paints OVER the top
            // strip while revealed. Header gestures (move drag, tab tear-out,
            // rename, open overflow menu) pin it so a fast pointer can't
            // strand it mid-drag. The content INTERACT rect stays below the
            // strip: whenever the pointer is up there the header is showing
            // and owns that band, so nothing is lost.
```

- [ ] **Step 5: Run tests**

Run: `cargo test 2>&1 | tail -3`
Expected: `374 passed; 0 failed` (373 + the new one).

- [ ] **Step 6: Commit**

```bash
git add src/wm.rs
git commit -m "$(cat <<'EOF'
feat(wm): one top-band hover-reveal rule for project and terminal chrome

Projects drop the always-on escape hatch; terminals narrow from
anywhere-on-window to the shared band rule. reveal_band is the pure,
tested geometry seam; occlusion stays on ui.rect_contains_pointer.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Transparent project header + `⋯` overflow menu

**Files:**
- Modify: `src/wm.rs:2641-2647` (title bg fill)
- Modify: `src/wm.rs:2950-2995` (delete the PS·CMD·SH shell-chip row for projects)
- Modify: `src/wm.rs:2997-3126` (project window controls → `✕` + `⋯` only;
  new-project `+` button removed; terminals keep all four controls)
- Test: none new (menu items reuse existing `Act` paths, already covered);
  evidence is screenshots in Task 4.

**Interfaces:**
- Consumes: menu-open flag id `base.with((id, "ovfmenu_open"))` from Task 1.
- Produces: overflow menu popup with items mapped to existing Acts:
  `Act::AddTerm(id, Shell::{PowerShell,Cmd,Bash})`, `Act::OpenProjectPicker`,
  `Act::Float(id)`, `Act::Min(id)`, `Act::Max(id)`. No new Act variants.

- [ ] **Step 1: Kill the project title bg fill**

At `:2641-2647`, the branch currently fills the title strip for both kinds.
Make it terminal-only:

```rust
            if reveal_chrome {
                // Projects have NO title bg — quiet chrome (spec 2026-07-02):
                // the band reads as window margin until revealed, and revealed
                // chrome floats directly on WIN_BG. Terminals keep their fill.
                if !is_project {
                    p.rect_filled(
                        title_rect,
                        cr,
                        if is_focus { TITLE_BG_FOCUS } else { TITLE_BG },
                    );
                }
```

(The `(tbg, tbg_focus)` tuple selection at `:2642-2646` is deleted.)

- [ ] **Step 2: Delete the project shell-chip row**

Delete the whole `if is_project { ... }` block at `:2950-2995` (the
`PS / CMD / SH` chips that push `Act::AddTerm`). The same actions reappear in
the menu below.

- [ ] **Step 3: Project controls become `✕` + `⋯`; terminals unchanged**

The window-controls loop at `:3001-3102` iterates
`[("close", true), ("max", false), ("min", false), ("float", false)]`.
Split by kind — keep the existing loop body (icon painting, Act pushes)
intact, but drive it from a kind-dependent role list, and add an `"ovf"`
role for projects:

```rust
                // --- window controls ---
                // Projects show only close + overflow (quiet chrome); the
                // remaining actions live in the ⋯ menu. Terminals keep all
                // four buttons.
                let roles: &[(&str, bool)] = if is_project {
                    &[("close", true), ("ovf", false)]
                } else {
                    &[("close", true), ("max", false), ("min", false), ("float", false)]
                };
                let by = scr.min.y + 3.0;
                let bh = TITLE_H - 6.0;
                let mut bx = scr.max.x - 4.0 - 22.0;
                for (role, danger) in roles.iter().copied() {
```

In the icon `match`, add an arm for `"ovf"` (three dots, painted as filled
circles so weight matches the stroke icons):

```rust
                        "ovf" => {
                            for dx in [-4.0f32, 0.0, 4.0] {
                                p.circle_filled(
                                    egui::pos2(c.x + dx, c.y),
                                    1.2,
                                    if is_focus { TEXT } else { DIM },
                                );
                            }
                        }
```

And in the click `match`, `"ovf"` toggles the menu flag instead of pushing an
Act:

```rust
                    if resp.clicked() {
                        match role {
                            "close" => acts.push(Act::Close(id)),
                            "max" => acts.push(Act::Max(id)),
                            "float" => acts.push(Act::Float(id)),
                            "ovf" => {
                                let mid = base.with((id, "ovfmenu_open"));
                                ui.ctx().data_mut(|d| {
                                    let open = d.get_temp::<bool>(mid).unwrap_or(false);
                                    d.insert_temp(mid, !open);
                                });
                            }
                            _ => acts.push(Act::Min(id)),
                        }
                    }
```

Remember the `⋯` button rect for the click-outside test in Step 5:
immediately after the loop body computes `r` for the `"ovf"` role, capture it:

```rust
                let mut ovf_rect = egui::Rect::NOTHING;
```
(declare before the loop; inside the loop, first line after `r` is computed:)
```rust
                    if role == "ovf" {
                        ovf_rect = r;
                    }
```

- [ ] **Step 4: Delete the new-project `+` button**

Delete the `if is_project { ... }` block at `:3104-3126`
(`Act::OpenProjectPicker` via the `+`). It moves into the menu.

- [ ] **Step 5: The overflow menu popup**

Directly after the controls loop (inside the `if reveal_chrome` branch, still
inside the project arm), render the menu when open. Items are painted rows —
house style (painter + interact), not egui widgets:

```rust
                // --- ⋯ overflow menu (projects only) ---
                // Open flag lives in transient egui memory: per-frame UI
                // state, not model state — it must never reach Win/serde.
                if is_project {
                    let mid = base.with((id, "ovfmenu_open"));
                    let open = ui.ctx().data(|d| d.get_temp::<bool>(mid)).unwrap_or(false);
                    if open {
                        let float_label = if is_tiled { "Float" } else { "Tile" };
                        let labels = [
                            "New PS terminal",
                            "New CMD terminal",
                            "New SH terminal",
                            "New project",
                            float_label,
                            "Minimize",
                            "Maximize",
                        ];
                        let font = egui::FontId::proportional(12.0);
                        let row_h = 22.0;
                        let pad = 10.0;
                        let w = labels
                            .iter()
                            .map(|l| {
                                ui.painter()
                                    .layout_no_wrap((*l).to_owned(), font.clone(), TEXT)
                                    .size()
                                    .x
                            })
                            .fold(0.0f32, f32::max)
                            + pad * 2.0;
                        let origin = egui::pos2(
                            (ovf_rect.right() - w).max(area.min.x),
                            ovf_rect.bottom() + 2.0,
                        );
                        let panel = egui::Rect::from_min_size(
                            origin,
                            egui::vec2(w, row_h * labels.len() as f32 + 8.0),
                        );
                        egui::Area::new(base.with((id, "ovfmenu")))
                            .order(egui::Order::Foreground)
                            .fixed_pos(origin)
                            .show(ui.ctx(), |mui| {
                                let mp = mui.painter();
                                mp.rect_filled(panel, egui::CornerRadius::same(4), TITLE_BG);
                                mp.rect_stroke(
                                    panel,
                                    egui::CornerRadius::same(4),
                                    egui::Stroke::new(1.0, BORDER),
                                    egui::StrokeKind::Inside,
                                );
                                for (ri, label) in labels.iter().enumerate() {
                                    let rr = egui::Rect::from_min_size(
                                        egui::pos2(panel.min.x, panel.min.y + 4.0 + row_h * ri as f32),
                                        egui::vec2(w, row_h),
                                    );
                                    let rresp = mui.interact(
                                        rr,
                                        base.with((id, "ovfitem", ri)),
                                        egui::Sense::click(),
                                    );
                                    if rresp.hovered() {
                                        mui.painter().rect_filled(rr, 0.0, TITLE_BG_FOCUS);
                                    }
                                    mui.painter().text(
                                        egui::pos2(rr.min.x + pad, rr.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        *label,
                                        font.clone(),
                                        TEXT,
                                    );
                                    if rresp.clicked() {
                                        match ri {
                                            0 => acts.push(Act::AddTerm(id, Shell::PowerShell)),
                                            1 => acts.push(Act::AddTerm(id, Shell::Cmd)),
                                            2 => acts.push(Act::AddTerm(id, Shell::Bash)),
                                            3 => acts.push(Act::OpenProjectPicker),
                                            4 => acts.push(Act::Float(id)),
                                            5 => acts.push(Act::Min(id)),
                                            _ => acts.push(Act::Max(id)),
                                        }
                                        mui.ctx().data_mut(|d| d.insert_temp(mid, false));
                                    }
                                }
                            });
                        // Click anywhere outside menu + button, or Escape: close.
                        let clicked_out = ui.input(|i| {
                            i.pointer.any_click()
                                && i.pointer
                                    .latest_pos()
                                    .is_some_and(|p| !panel.contains(p) && !ovf_rect.contains(p))
                        });
                        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if clicked_out || esc {
                            ui.ctx().data_mut(|d| d.insert_temp(mid, false));
                        }
                    }
                }
```

NOTE: `acts` is captured by the closure — the menu rows push the SAME Acts
the deleted buttons pushed, so `apply_acts` and all its tests are untouched.
If the borrow checker rejects pushing to `acts` inside the `show` closure
(it borrows `ui` mutably too), collect clicked `ri` into a
`let mut menu_click: Option<usize> = None;` inside the closure and do the
`match ri` push after `.show(...)` returns.

- [ ] **Step 6: Build + full tests**

Run: `cargo test 2>&1 | tail -3` then `cargo build 2>&1 | tail -3`
Expected: `374 passed`, build finishes with the pre-existing 4 warnings only.

- [ ] **Step 7: Commit**

```bash
git add src/wm.rs
git commit -m "$(cat <<'EOF'
feat(wm): transparent project header with overflow menu

Project chrome collapses to [name] [tabs] [⋯] [✕]: no title bg fill,
shell chips / + project / float / min / max move into the ⋯ popup.
Menu rows push the same Acts the buttons did; open flag is transient
egui memory, never model state. Terminals keep their styling.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Fade animation

**Files:**
- Modify: `src/wm.rs` header branch (`if reveal_chrome { ... }`, Task-2 state)

**Interfaces:**
- Consumes: `reveal_chrome` from Task 1.
- Produces: nothing downstream; purely visual.

- [ ] **Step 1: Alpha from `animate_bool`**

Immediately after `reveal_chrome` is computed, add:

```rust
            // Chrome alpha: 0→1 fade, same feel as the OS bar minus the slide
            // (the band is reserved space; sliding would be motion for its
            // own sake). Interacts stay inside `if reveal_chrome`, so a
            // fading-out header never swallows a click.
            let chrome_t = ui.ctx().animate_bool(base.with((id, "chrome_t")), reveal_chrome);
```

Change the branch gate from `if reveal_chrome {` to `if chrome_t > 0.0 {`,
and inside it define:

```rust
                let fade = |c: egui::Color32| c.gamma_multiply(chrome_t);
```

- [ ] **Step 2: Apply `fade()` to every header paint**

Wrap the color argument of every paint call inside the header branch:
the terminal title bg fill (Task 2 Step 1), title text, tab chips and their
close ✕, the rename field frame, the window-control icon strokes and hover
bgs, and the `⋯` dots. Pattern:

```rust
// before
p.rect_filled(title_rect, cr, if is_focus { TITLE_BG_FOCUS } else { TITLE_BG });
// after
p.rect_filled(title_rect, cr, fade(if is_focus { TITLE_BG_FOCUS } else { TITLE_BG }));
```

Strokes: `egui::Stroke::new(1.4, fade(if is_focus { TEXT } else { DIM }))`.
Do NOT fade the overflow menu panel — it only exists while `reveal_chrome`
is pinned true (menu open), so it is always at full alpha; fading it would
just dim the first frame.

Interactive affordances (all `ui.interact` calls in the branch) move under a
nested `if reveal_chrome { ... }` only if they aren't already gated — with the
branch now entered during fade-out (`chrome_t > 0.0` but `reveal_chrome`
false), guard the `resp.clicked()` handlers:

```rust
                    if reveal_chrome && resp.clicked() {
```

(one-word change per handler; there are four sites after Task 2: window
controls, tab chips, tab close ✕, rename start).

- [ ] **Step 3: Build + tests + eyeball**

Run: `cargo test 2>&1 | tail -3`; expected `374 passed`.
Run `cargo build`, launch, and confirm by moving the pointer into and out of
a title band: chrome fades in/out smoothly, no flicker, menu unaffected.

- [ ] **Step 4: Commit**

```bash
git add src/wm.rs
git commit -m "$(cat <<'EOF'
feat(wm): fade window chrome in and out on band hover

animate_bool alpha over every header paint; click handlers gated on
reveal_chrome so a fading-out header never eats a click.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Evidence pass, focus-legibility check, docs

**Files:**
- Create: `docs/window-chrome.md`
- Modify: `CLAUDE.md` (one line in the `src/wm.rs` architecture bullet)
- Modify (contingency only): `src/wm.rs` focused project border width

**Interfaces:**
- Consumes: everything above. Produces: the feature-doc gate artifacts.

- [ ] **Step 1: Temp-seed three projects for screenshots**

In `src/main.rs`, inside the `if !self.started` block, TEMPORARILY add two
more `add_project` + `tile_new` calls after the existing one (same pattern,
same `dir`). This is the sanctioned no-mouse-hijack technique (HANDOFF §3);
it MUST be reverted in Step 4.

- [ ] **Step 2: Screenshot hidden and revealed states**

Build and capture (PowerShell; kill foreman first):

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 3
$p = Start-Process -FilePath ".\target\debug\foreman.exe" -PassThru
Start-Sleep -Seconds 6
Add-Type @"
using System; using System.Runtime.InteropServices;
public class Cap { [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  public struct RECT { public int Left, Top, Right, Bottom; } }
"@
[Cap]::SetForegroundWindow($p.MainWindowHandle) | Out-Null; Start-Sleep -Milliseconds 400
$r = New-Object Cap+RECT; [Cap]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null
Add-Type -AssemblyName System.Drawing
$b = New-Object System.Drawing.Bitmap(($r.Right-$r.Left), ($r.Bottom-$r.Top))
$g = [System.Drawing.Graphics]::FromImage($b); $g.CopyFromScreen($r.Left,$r.Top,0,0,$b.Size)
$b.Save("$(Get-Location)\win_hidden.png"); $g.Dispose(); $b.Dispose()
```

`Read` `win_hidden.png`. Checks: NO project header bg visible, three tiled
projects, focused project's border clearly distinguishable. If focus is NOT
legible, apply the contingency: in the border paint, use width `2.0` instead
of `BORDER_W` when `is_focus && is_project`, rebuild, re-shoot.

For the revealed + menu-open states, TEMPORARILY force them (pointer can't be
scripted without hijacking the mouse): set `let reveal_chrome = true;` after
the real computation, and default the menu flag read to `true` for the menu
shot. Rebuild, capture `win_revealed.png` and `win_menu.png`, `Read` both.
Checks: header shows `[name] [tabs] … [⋯] [✕]` only; menu lists the seven
items right-aligned under `⋯`. REVERT both temp lines.

- [ ] **Step 3: Live interactive check**

Launch normally and hand it to the user for feel (band reveal latency, fade,
menu). Do not SendKeys/mouse_event — user acceptance is the evidence here.

- [ ] **Step 4: Revert temp seeds and temp forces**

`git diff src/main.rs src/wm.rs` must show ONLY the intended feature changes;
the seed and force lines are gone. `cargo test 2>&1 | tail -3` → `374 passed`.

- [ ] **Step 5: Feature doc + CLAUDE.md line**

Create `docs/window-chrome.md`:

```markdown
# Window chrome: hover-reveal and the project overflow menu

One reveal rule for BOTH window kinds (desktop projects and in-project
terminals): chrome appears while the pointer is in the window's title band
(`reveal_band` — the top `TITLE_H` strip clipped to the manager area), or
while a header gesture pins it (move drag, rename, tab tear-out, open
overflow menu). Reveal is instant; hide is an alpha fade
(`animate_bool` × `gamma_multiply`). The hover test is
`ui.rect_contains_pointer`, which is layer-aware — never replace it with a
raw rect/pointer test or occluded windows reveal through floats.

Projects paint no title bg (quiet chrome): revealed layout is
`[name] [tab chips] … [⋯] [✕]`. The ⋯ popup (transient egui memory flag,
never model state) holds: New PS/CMD/SH terminal, New project, Float/Tile,
Minimize, Maximize — each pushing the same `Act` the old header buttons
pushed. ✕ stays first-class. Terminals keep their fill and buttons.

Focus reads from the border alone for projects (`PROJ_BORDER_FOCUS`).

## Key files

- `src/wm.rs` — `reveal_band`, the `reveal_chrome`/`chrome_t` computation,
  the header paint branch, the overflow menu popup.
- `docs/superpowers/specs/2026-07-02-quiet-project-chrome-design.md` — the
  approved design and its rejected alternatives.
```

In `CLAUDE.md`, extend the `src/wm.rs` bullet with one sentence:
`Chrome at both levels is band-hover-revealed with an alpha fade; project
headers are transparent with a ⋯ overflow menu (docs/window-chrome.md).`

- [ ] **Step 6: Final commit**

```bash
git add docs/window-chrome.md CLAUDE.md
git commit -m "$(cat <<'EOF'
docs(wm): window-chrome feature doc + CLAUDE.md pointer

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

## Self-review notes (done at write time)

- Spec coverage: §1→Task 1, §2/§3→Task 2, §4→Task 3, §5→Task 4 Step 2
  contingency, §6 honored (terminal styling untouched), Testing→Task 1 tests
  + Task 4 evidence. No gaps.
- The spec's "interact only while t > 0.5" is implemented strictly as
  "handlers gated on `reveal_chrome`" (Task 3 Step 2) — stronger and simpler;
  spec intent (no clicks into a fading header) preserved.
- Type consistency: menu flag id `base.with((id, "ovfmenu_open"))` identical
  in Tasks 1 and 2; `reveal_band(scr, area)` signature identical in Tasks 1
  and 4 doc.
