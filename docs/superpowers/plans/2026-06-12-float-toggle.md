# Float Toggle Button + Shift-Gated Snapping Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A float/tile toggle button in every window header, and free-move drags for floating windows (tree snapping only on tear-out drags or while Shift is held).

**Architecture:** All changes live in `src/wm.rs` (the shared window engine), so both compositor levels get them automatically. The existing `toggle_float` (leader `F`) is generalized to take a `WinId` and wired to a new header button via a new `Act::Float`. Drop-hint/drop-commit logic in `show()` is gated on a new drag-origin field (`drag_from_tree`) OR the Shift modifier.

**Tech Stack:** Rust, egui 0.34. Windows/PowerShell, GNU toolchain (see CLAUDE.md gotchas — kill the app before building).

**Spec:** `docs/superpowers/specs/2026-06-12-float-toggle-design.md`

**Project conventions that apply here:**
- This project uses Serena MCP tools for code reads/edits (`find_symbol`, `replace_content`, `replace_symbol_body`) — prefer them over raw Read/Edit on `src/*.rs`.
- Build requires killing the app first: `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue`.
- `cargo test` does not need the kill (separate test binary).

---

### Task 1: `toggle_float_for(id)` — id-targeted float toggle

The header button acts on the **clicked** window, which need not be the focused one. Generalize `toggle_float` to take an id; keep a thin wrapper for the leader-key path.

**Files:**
- Modify: `src/wm.rs` — `WindowManager::toggle_float`, plus a new test in `mod tests`
- Test: `src/wm.rs` (tests live in the same file, `mod tests` at the bottom)

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/wm.rs`, right after `toggle_float_roundtrips_tree_membership_and_rect` (use its helpers — `push` creates a stub window and returns its `WinId`):

```rust
    #[test]
    fn toggle_float_for_targets_an_unfocused_window_and_focuses_it() {
        // The header button acts on the clicked window, not the focused one.
        let mut wm = WindowManager::new();
        let a = push(&mut wm, "A");
        let b = push(&mut wm, "B");
        wm.last_area = egui::vec2(1000.0, 800.0);
        wm.tree.insert_root(a, Dir::Right); // a tiled, b floating
        wm.focus(b);

        wm.toggle_float_for(a);
        assert!(!wm.tree.contains(a), "a detached from tree");
        assert_eq!(wm.focused, Some(a), "toggle focuses the toggled window");

        wm.toggle_float_for(a);
        assert!(wm.tree.contains(a), "a re-entered the tree");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test toggle_float_for_targets -- --nocapture`
Expected: compile error — `no method named 'toggle_float_for' found`

- [ ] **Step 3: Implement**

Replace the whole `toggle_float` method on `WindowManager` (currently `fn toggle_float(&mut self)`) with:

```rust
    /// Toggle the focused window between tiled and floating (leader F / Ctrl+F).
    fn toggle_float(&mut self) {
        if let Some(id) = self.focused {
            self.toggle_float_for(id);
        }
    }

    /// Toggle `id` between tiled and floating. Un-tiling restores the remembered
    /// floating rect; re-tiling enters the tree where the window currently sits
    /// (the leaf under its center, split along its longer axis). Focuses `id`.
    fn toggle_float_for(&mut self, id: WinId) {
        if self.tree.contains(id) {
            self.detach(id);
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                w.rect = w.prev.take().unwrap_or(egui::Rect::from_min_size(
                    egui::pos2(60.0, 60.0),
                    egui::vec2(580.0, 380.0),
                ));
            }
        } else {
            let (center, rect) = match self.windows.iter().find(|w| w.id == id) {
                Some(w) => (w.rect.center(), w.rect),
                None => return,
            };
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                w.prev = Some(rect);
            }
            let local = egui::Rect::from_min_size(egui::Pos2::ZERO, self.last_area);
            match self.tree.hit_leaf(center, local, SNAP_GAP) {
                Some((leaf, r)) => {
                    let side = if r.width() >= r.height() { Dir::Right } else { Dir::Down };
                    self.tree.insert_split(leaf, id, side);
                }
                None => self.tree.insert_root(id, Dir::Right),
            }
        }
        self.focus(id);
    }
```

This is the existing body verbatim minus the `let Some(id) = self.focused else { return };` line. The two `dispatch` call sites (`Command::ProjFloat => self.toggle_float()`, `Command::TermFloat => child.toggle_float()`) and the existing roundtrip test keep compiling unchanged through the wrapper.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test toggle_float -- --nocapture`
Expected: PASS — both `toggle_float_roundtrips_tree_membership_and_rect` and the new test.

- [ ] **Step 5: Commit (includes the spec + this plan)**

```powershell
git add src/wm.rs docs/superpowers/specs/2026-06-12-float-toggle-design.md docs/superpowers/plans/2026-06-12-float-toggle.md
git commit -m @'
refactor(wm): toggle_float takes a target id

Header-button groundwork: the button acts on the clicked window, the
leader-F path keeps its focused-window wrapper. Spec + plan included.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 2: Float/tile toggle button in the header controls

**Files:**
- Modify: `src/wm.rs` — `enum Act`, `WindowManager::apply_acts`, and the window-controls section of `WindowManager::show`

- [ ] **Step 1: Add the `Act::Float` variant**

In `enum Act`, add after the `Max(WinId)` variant:

```rust
    Float(WinId),
```

- [ ] **Step 2: Apply it in `apply_acts`**

In the `match a` inside `apply_acts`, add after the `Act::Max(id) => self.toggle_zoom(id),` arm:

```rust
                Act::Float(id) => self.toggle_float_for(id),
```

(`toggle_float_for` already ends in `self.focus(id)`, so the click also focuses.)

- [ ] **Step 3: Widen `ctl_w` for the fourth button**

In `show()`, the line

```rust
            let ctl_w = if is_project { 116.0 } else { 88.0 };
```

becomes

```rust
            let ctl_w = if is_project { 141.0 } else { 113.0 };
```

(+25 each — one more 22px button at the existing 25px pitch; preserves the current clearances for the title-drag rect, rename field, dispatch keys, and tab chips, which all key off `ctl_w`.)

- [ ] **Step 4: Add the button to the controls loop**

In the `// --- window controls ---` section of `show()`, the loop header

```rust
            for (role, danger) in [("close", true), ("max", false), ("min", false)] {
```

becomes (buttons draw right-to-left, so last = leftmost):

```rust
            for (role, danger) in [
                ("close", true),
                ("max", false),
                ("min", false),
                ("float", false),
            ] {
```

In the icon `match role { ... }` inside that loop, add a `"float"` arm before the `_ =>` (close) arm. `c`, `s`, `stroke`, `p`, and `is_tiled` are already in scope:

```rust
                    "float" => {
                        if is_tiled {
                            // In the tree: 2×2 grid. Click pops it out to floating.
                            p.rect_stroke(
                                egui::Rect::from_center_size(c, egui::vec2(s * 2.0, s * 2.0)),
                                egui::CornerRadius::same(1),
                                stroke,
                                egui::StrokeKind::Inside,
                            );
                            p.line_segment(
                                [egui::pos2(c.x, c.y - s), egui::pos2(c.x, c.y + s)],
                                stroke,
                            );
                            p.line_segment(
                                [egui::pos2(c.x - s, c.y), egui::pos2(c.x + s, c.y)],
                                stroke,
                            );
                        } else {
                            // Floating: two offset squares. Click tiles it
                            // (enters at the leaf under the window's center).
                            let q = s * 0.8;
                            let o = 1.5;
                            p.rect_stroke(
                                egui::Rect::from_center_size(
                                    egui::pos2(c.x + o, c.y - o),
                                    egui::vec2(q * 2.0, q * 2.0),
                                ),
                                egui::CornerRadius::same(1),
                                stroke,
                                egui::StrokeKind::Inside,
                            );
                            p.rect_stroke(
                                egui::Rect::from_center_size(
                                    egui::pos2(c.x - o, c.y + o),
                                    egui::vec2(q * 2.0, q * 2.0),
                                ),
                                egui::CornerRadius::same(1),
                                stroke,
                                egui::StrokeKind::Inside,
                            );
                        }
                    }
```

And the click dispatch at the bottom of the loop

```rust
                if resp.clicked() {
                    acts.push(match role {
                        "close" => Act::Close(id),
                        "max" => Act::Max(id),
                        _ => Act::Min(id),
                    });
                }
```

becomes

```rust
                if resp.clicked() {
                    acts.push(match role {
                        "close" => Act::Close(id),
                        "max" => Act::Max(id),
                        "float" => Act::Float(id),
                        _ => Act::Min(id),
                    });
                }
```

The project `+` button reads `bx` after the loop, so it automatically lands left of the new button. No change needed there.

- [ ] **Step 5: Build + test**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
cargo test 2>&1 | Select-Object -Last 10
```

Expected: clean build (no new warnings), all tests pass.

- [ ] **Step 6: Commit**

```powershell
git add src/wm.rs
git commit -m @'
feat(wm): float/tile toggle button in window headers

Left of minimize on every window (projects keep + to its left). Icon
shows current state: 2x2 grid = tiled, offset squares = floating.
Click = toggle_float_for the clicked window, same snap-to-nearest
semantics as leader F.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 3: Shift-gated snapping for floating drags

Drop semantics now depend on where the drag **started**. A per-frame `tree.contains(id)` check cannot express this — after tear-out the window is no longer in the tree but that drag must keep its hints — so the origin is tracked in a field.

**Files:**
- Modify: `src/wm.rs` — `WindowManager` struct, `WindowManager::new`, and the `dr.dragged()` / `dr.drag_stopped()` blocks in `show()`

- [ ] **Step 1: Add the drag-origin field**

In `struct WindowManager`, after the `zoomed` field:

```rust
    /// The window whose in-flight header drag started tiled/zoomed (tear-out).
    /// Such a drag keeps the tree drop hints without a modifier; a drag that
    /// started floating is a free move unless Shift is held.
    drag_from_tree: Option<WinId>,
```

In `WindowManager::new()`, after `zoomed: None,`:

```rust
            drag_from_tree: None,
```

- [ ] **Step 2: Record the origin on tear-out**

In `show()`, inside `if dr.dragged() {`, the opening

```rust
                let popped = self.tree.contains(id) || self.zoomed == Some(id);
                if popped {
                    self.detach(id);
                }
```

becomes

```rust
                let popped = self.tree.contains(id) || self.zoomed == Some(id);
                if popped {
                    self.detach(id);
                    self.drag_from_tree = Some(id);
                }
```

- [ ] **Step 3: Gate the in-flight hints**

Still inside `if dr.dragged() {`, replace the merge/hint detection block

```rust
                // --- merge target detection: is the pointer over another window? ---
                // Dropping a window's title onto another window tabs it onto that
                // window's stack. While hovering a merge target we suppress the snap
                // overlay and instead highlight the target (handled at paint time).
                let pointer = ui.ctx().pointer_latest_pos();
                let over_target = pointer.and_then(|p| self.merge_target_at(id, p, area, &order));
                if let Some(tgt) = over_target {
                    merge_hint = Some(tgt);
                } else if let Some(p) = pointer {
                    // Tree drop hint: leaf edges split, leaf centers tab-merge,
                    // area edge bands split the root. Painted like the old snap overlay.
                    if let Some((_, hint)) = self.tree.drop_target(p, area, SNAP_GAP) {
                        snap_overlay = Some(hint);
                    }
                }
```

with

```rust
                // --- merge target detection: is the pointer over another window? ---
                // Drop semantics are gated on drag origin: a tear-out (started
                // tiled/zoomed) keeps its hints; a drag that started floating is
                // a pure move unless Shift opts in. Checked live each frame so
                // pressing/releasing Shift mid-drag lights hints up and down.
                let snap_ok = self.drag_from_tree == Some(id)
                    || ui.input(|inp| inp.modifiers.shift);
                let pointer = ui.ctx().pointer_latest_pos();
                let over_target = if snap_ok {
                    pointer.and_then(|p| self.merge_target_at(id, p, area, &order))
                } else {
                    None
                };
                if let Some(tgt) = over_target {
                    merge_hint = Some(tgt);
                } else if snap_ok {
                    if let Some(p) = pointer {
                        // Tree drop hint: leaf edges split, leaf centers tab-merge,
                        // area edge bands split the root.
                        if let Some((_, hint)) = self.tree.drop_target(p, area, SNAP_GAP) {
                            snap_overlay = Some(hint);
                        }
                    }
                }
```

- [ ] **Step 4: Gate the drop commit and clear the flag**

In `show()`, the `if dr.drag_stopped() {` block's opening

```rust
            if dr.drag_stopped() {
                let pointer = ui.ctx().pointer_latest_pos();
                // A drop onto another window's titlebar merges (tabs) onto it and wins
                // over the tree drop: the dragged window is consumed entirely.
                let merge_dst = pointer.and_then(|p| self.merge_target_at(id, p, area, &order));
                if let Some(dst_i) = merge_dst {
                    let dst = self.windows[dst_i].id;
                    acts.push(Act::Merge { src: id, dst });
                } else if let Some(p) = pointer {
```

becomes

```rust
            if dr.drag_stopped() {
                // Drag origin decides drop rights (Shift overrides for floating
                // drags). take() clears the flag at end-of-gesture either way.
                let snap_ok = self.drag_from_tree.take() == Some(id)
                    || ui.input(|inp| inp.modifiers.shift);
                let pointer = ui.ctx().pointer_latest_pos();
                // A drop onto another window's titlebar merges (tabs) onto it and wins
                // over the tree drop: the dragged window is consumed entirely.
                let merge_dst = if snap_ok {
                    pointer.and_then(|p| self.merge_target_at(id, p, area, &order))
                } else {
                    None
                };
                if let Some(dst_i) = merge_dst {
                    let dst = self.windows[dst_i].id;
                    acts.push(Act::Merge { src: id, dst });
                } else if snap_ok {
                    if let Some(p) = pointer {
```

The body inside (the `drop_target` match committing Merge/Split/Root) is unchanged, but the new `if let Some(p) = pointer {` nesting adds one brace level — close it with an extra `}` at the end of that `else if` arm. The `// Rect refits from the tree next frame` comment stays with the match.

Note: a tab dragged off a stack (`Act::Untab` with `grab`) hands the gesture to a brand-new floating window whose drag never "started" anywhere tiled — under this rule the rest of that gesture is a free move, Shift opts into snapping. That is intended; it gets a doc note in Task 4.

- [ ] **Step 5: Build + test**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
cargo test 2>&1 | Select-Object -Last 10
```

Expected: clean build, all tests pass. (The gating itself is egui interaction code with no test harness — per spec it is verified live in Task 5.)

- [ ] **Step 6: Commit**

```powershell
git add src/wm.rs
git commit -m @'
feat(wm): floating drags move free; Shift opts into drop snapping

Tear-out drags (started tiled/zoomed) keep the drop hints as before.
Drags that start on a floating window are pure moves: no hints, no
tree insert, no titlebar tab-merge - unless Shift is held, which
enables all drop semantics live. Origin tracked in drag_from_tree;
tree membership can't express it after the tear-out detaches.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 4: Help overlay + docs

**Files:**
- Modify: `src/wm.rs` — the `Drag` row in `WindowManager::paint_help`
- Modify: `docs/tiling-tree.md` — Mouse section + Gotchas

- [ ] **Step 1: Update the help overlay drag rows**

In `paint_help`, replace

```rust
        rows.push((String::new(), String::new()));
        rows.push((
            "  Drag".into(),
            "drag a header \u{2014} leaf edges split, centers stack as tabs, screen edges make a column".into(),
        ));
```

with

```rust
        rows.push((String::new(), String::new()));
        rows.push((
            "  Drag".into(),
            "tiled: tear out, hints place it \u{2014} floating: free move".into(),
        ));
        rows.push((
            "  Shift+Drag".into(),
            "floating window shows drop hints and snaps into the tree".into(),
        ));
```

- [ ] **Step 2: Update `docs/tiling-tree.md`**

Replace the `Mouse:` bullet list with:

```markdown
Mouse:

- **Float/tile toggle button** in every header, left of minimize (projects
  keep `+` to its left). The icon shows the current state: 2×2 grid = tiled,
  two offset squares = floating. Clicking toggles with the same semantics as
  leader `F` — popping back in enters the tree at the leaf under the
  window's center.
- **Drag a tiled window's header** → it tears out of the tree instantly
  (siblings absorb the space) and floats under the cursor. A tear-out drag
  keeps its amber drop hints for the whole gesture:
  - edge half of a tile → split that tile on that side
  - center of a tile → merge as a tab onto that window
  - thin band at the area edge → split the whole root (full row/column)
  - drop on another window's **titlebar** → tab-merge (wins over tree hints)
- **Drag a floating window's header** → pure free move: no hints, nothing
  happens on drop. Hold **Shift** at any point during the drag to enable the
  full drop semantics above (hints light up while held).
- **Drag a shared edge** between tiles → moves that divider (adjusts tree
  ratios; clamped so no tile drops below 10% of its split). Dragging the
  OUTER edge of a tile does nothing — tear-out lives on the header drag.
```

Append to the `## Gotchas` list:

```markdown
- **Drop gating keys off where the drag STARTED** (`WindowManager.
  drag_from_tree`), not current tree membership — after tear-out the window
  is already floating, but that drag keeps its hints. A per-frame
  `tree.contains` check would kill the hints one frame into every tear-out.
- **A tab dragged off a stack** becomes a new floating window mid-gesture
  (`Act::Untab` + grab), so the rest of that drag is a free move — hold
  Shift to snap it into the tree in the same gesture.
```

- [ ] **Step 3: Build + test (the help overlay change is code)**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
cargo test 2>&1 | Select-Object -Last 10
```

Expected: clean build, all tests pass.

- [ ] **Step 4: Commit**

```powershell
git add src/wm.rs docs/tiling-tree.md
git commit -m @'
docs: float toggle button + Shift-gated snapping

Help overlay drag rows reworded; tiling-tree doc covers the new
button, free-move floating drags, and the drag-origin gotchas.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
'@
```

---

### Task 5: Visual verification

The button rendering must be verified by screenshot (working agreement: no success claims without evidence). The drag *feel* needs a human mouse — hand that to the user rather than hijacking their pointer.

**Files:** none (verification only; temporary `src/main.rs` edit is reverted)

- [ ] **Step 1: Spawn test windows at startup (temporary)**

In `src/main.rs`, in the `if !self.started` block, temporarily add a project + two terminals (per HANDOFF.md § 3: call `add_project` / the terminal-spawn helpers used there) so the screenshot shows a tiled window AND a floating one. Use leader-key-free state: one terminal tiles by default; mark this edit clearly `// TEMP: verify float toggle`.

- [ ] **Step 2: Build, run, screenshot**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 20
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
$b.Save("$(Get-Location)\win.png"); $g.Dispose(); $b.Dispose()
```

Then `Read` `win.png` and confirm:
- four controls in terminal headers, five in project headers (`+` leftmost)
- the new icon reads as a grid on tiled windows, offset squares on floating ones
- nothing in the titlebar (title, tabs, dispatch chips) collides with the wider control cluster

- [ ] **Step 3: Revert the temporary `main.rs` edit**

```powershell
git checkout -- src/main.rs
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue
```

- [ ] **Step 4: Final full test run**

Run: `cargo test 2>&1 | Select-Object -Last 10`
Expected: all tests pass.

- [ ] **Step 5: Hand the feel-test to the user**

Ask the user to verify with their own mouse: (1) drag a floating window — no hints, no snap on drop; (2) Shift mid-drag — hints appear, drop snaps; (3) drag a tiled window — hints as before; (4) click the toggle both ways. Do NOT drive their mouse via `mouse_event`.

---

## Self-review notes

- Spec coverage: button (Task 2), id-targeted toggle (Task 1), drag gating incl. titlebar-merge gating (Task 3), help/docs (Task 4), ctl_w widths (Task 2 Step 3), zoom = unchanged `toggle_float` semantics (Task 1 keeps the body verbatim), verification split per spec (unit test Task 1, screenshot Task 5).
- Types consistent: `toggle_float_for(id: WinId)`, `Act::Float(WinId)`, `drag_from_tree: Option<WinId>` throughout.
