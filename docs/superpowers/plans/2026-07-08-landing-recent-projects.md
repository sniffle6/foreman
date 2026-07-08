# Landing Recent-Projects List Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A 5-entry MRU "Recent" list on the landing page: each entry remembers how it was opened (claude/codex/terminal) and one click — or Tab/arrows/Enter — fully reopens the project.

**Architecture:** New `src/recents.rs` owns the MRU model + `recents.json` persistence. `WindowManager` gains a passive open drain (`take_opened`) pushed by `add_project`/`add_project_with_command`; `App` drains it each frame and records. The landing renders the band from a pure `layout()` extension and drives keyboard focus with a pure `step()` state machine. Spec: `docs/superpowers/specs/2026-07-08-landing-recent-projects-design.md` (read it first).

**Tech Stack:** Rust, egui 0.34, serde/serde_json (already deps).

## Global Constraints

- Windows, GNU toolchain (`stable-gnu`). Kill the app before building or the link fails: `Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue`
- Build/test: `cargo build 2>&1 | Select-Object -Last 20`, `cargo test <filter>`
- **No new dependencies.**
- Colors only from `theme.rs` tokens (glob-imported); no ad-hoc RGB.
- egui 0.34: measure text via `ui.painter().layout_no_wrap(...)`, never `ui.fonts(|f| …)`.
- Kind strings are exactly `"claude"`, `"codex"`, `"terminal"` (lowercase).
- `MAX_RECENTS = 5` — store 5, show 5, one number.
- Commit messages: `type(scope): subject`, ending with trailer
  `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`

---

### Task 1: `src/recents.rs` — MRU model + persistence

**Files:**
- Create: `src/recents.rs`
- Modify: `src/main.rs:1-24` (add `mod recents;` to the module list, alphabetical — after `mod proc;`)
- Test: inline `#[cfg(test)] mod tests` in `src/recents.rs`

**Interfaces:**
- Consumes: `crate::config::{load_json, save_json}` (exist, generic over file name; `load_json` needs `T: DeserializeOwned + Default`).
- Produces (later tasks rely on these exact names):
  - `pub struct RecentEntry { pub path: PathBuf, pub kind: String }`
  - `pub struct Recents` with `pub fn load() -> Self`, `pub fn record(&mut self, path: PathBuf, kind: &str)`, `pub fn entries(&self) -> &[RecentEntry]`
  - `pub const MAX_RECENTS: usize = 5`
  - `pub fn kind_of_command(cmd: Option<&str>) -> &'static str`

- [ ] **Step 1: Write the failing tests**

Create `src/recents.rs` with only the test module (types come in step 3, so this fails to compile — that is the failure signal for pure-Rust TDD here):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn push_is_mru_with_case_insensitive_dedup_and_cap() {
        let mut r = Recents::default();
        r.push(PathBuf::from("H:\\Foo"), "terminal");
        r.push(PathBuf::from("h:\\foo"), "claude"); // same project on Windows
        assert_eq!(r.entries().len(), 1, "case-folded dedup");
        assert_eq!(r.entries()[0].kind, "claude", "re-record adopts the new kind");
        assert_eq!(r.entries()[0].path, PathBuf::from("h:\\foo"));
        for i in 0..10 {
            r.push(PathBuf::from(format!("C:\\p{i}")), "terminal");
        }
        assert_eq!(r.entries().len(), MAX_RECENTS, "capped");
        assert_eq!(r.entries()[0].path, PathBuf::from("C:\\p9"), "most-recent-first");
    }

    #[test]
    fn unknown_kind_strings_survive_load() {
        // A future build may write kinds this build doesn't know. Kind is a
        // plain String precisely so the file still parses (spec amendment).
        let r: Recents =
            serde_json::from_str(r#"{"entries":[{"path":"H:\\x","kind":"future-agent"}]}"#)
                .unwrap();
        assert_eq!(r.entries()[0].kind, "future-agent");
    }

    #[test]
    fn empty_object_loads_as_default() {
        let r: Recents = serde_json::from_str("{}").unwrap();
        assert!(r.entries().is_empty());
    }

    #[test]
    fn kind_of_command_maps_stems() {
        assert_eq!(kind_of_command(None), "terminal");
        assert_eq!(kind_of_command(Some("claude")), "claude");
        assert_eq!(kind_of_command(Some("codex")), "codex");
        assert_eq!(kind_of_command(Some("some-other-tool --flag")), "terminal");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test recents 2>&1 | Select-Object -Last 10`
Expected: compile error — `Recents`, `MAX_RECENTS`, `kind_of_command` not found. (Add `mod recents;` to `src/main.rs` now or this file isn't compiled at all.)

- [ ] **Step 3: Implement the module**

Prepend above the test module:

```rust
//! Recent-projects MRU, persisted to `%APPDATA%\foreman\recents.json` via
//! config.rs's tolerant loader (missing/corrupt file → empty list). A separate
//! file from settings.json on purpose: settings are *preferences* written on a
//! zoom debounce, this is *state* written on project opens — keeping them apart
//! avoids interleaved writes. Spec:
//! docs/superpowers/specs/2026-07-08-landing-recent-projects-design.md

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const RECENTS_FILE: &str = "recents.json";
/// Store 5, show 5 — one number (grug-review amendment; no ghost entries).
pub const MAX_RECENTS: usize = 5;

/// One remembered open. `kind` is a plain string ("claude" | "codex" |
/// "terminal") — deliberately NOT the landing's provisional `SessionKind`, so
/// the disk format survives phase-2 renaming that enum, an unknown kind can
/// never fail the parse, and this module doesn't depend on a UI module.
/// Unknown strings degrade to Terminal at the landing edge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecentEntry {
    pub path: PathBuf,
    pub kind: String,
}

/// The MRU list. Mutation is pure (`push`); `record` adds persistence.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Recents {
    entries: Vec<RecentEntry>,
}

impl Recents {
    /// Load once at startup; corruption-tolerant per config.rs.
    pub fn load() -> Self {
        crate::config::load_json(RECENTS_FILE)
    }

    /// Record an open and persist. Best-effort: a failed save is logged and
    /// never blocks the open that triggered it.
    pub fn record(&mut self, path: PathBuf, kind: &str) {
        self.push(path, kind);
        if let Err(e) = crate::config::save_json(RECENTS_FILE, self) {
            eprintln!("foreman: could not save recents: {e}");
        }
    }

    /// Pure MRU mutation (split from `record` so tests never touch a disk):
    /// dedup by case-folded path, insert at front, cap.
    fn push(&mut self, path: PathBuf, kind: &str) {
        let key = fold(&path);
        self.entries.retain(|e| fold(&e.path) != key);
        self.entries.insert(
            0,
            RecentEntry {
                path,
                kind: kind.to_string(),
            },
        );
        self.entries.truncate(MAX_RECENTS);
    }

    /// Most-recent-first. Callers filter for display (e.g. missing dirs) —
    /// this module never touches the filesystem beyond its own JSON file.
    pub fn entries(&self) -> &[RecentEntry] {
        &self.entries
    }
}

/// Dedup key: Windows paths are case-insensitive but `PathBuf` equality is
/// not — `H:\Foo` and `h:\foo` are the same project. Lossy+lowercase is
/// deliberate: no filesystem calls (canonicalize) in the model.
fn fold(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

/// Kind string for a drained open: `None` (plain shell) → terminal, otherwise
/// the injected command's first token. Matches the strings
/// `SessionKind::launch_command` produces; anything unrecognized is terminal
/// (honest fallback — never guess, per grug review).
pub fn kind_of_command(cmd: Option<&str>) -> &'static str {
    match cmd.and_then(|c| c.split_whitespace().next()) {
        Some("claude") => "claude",
        Some("codex") => "codex",
        _ => "terminal",
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test recents 2>&1 | Select-Object -Last 10`
Expected: `4 passed; 0 failed` (plus the rest of the suite untouched).

- [ ] **Step 5: Commit**

```powershell
git add src/recents.rs src/main.rs
git commit -m "feat(recents): MRU model + recents.json persistence

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: `wm.rs` open drain

**Files:**
- Modify: `src/wm.rs` — `WindowManager` struct fields + its `new()`; `add_project` (~line 777); `add_project_with_command` (~line 798); wm tests module
- Test: inline in `src/wm.rs`'s existing `#[cfg(test)]` tests module

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub fn take_opened(&mut self) -> Vec<(PathBuf, Option<String>)>` on `WindowManager` — `(project cwd, injected command if any)`, drained by Task 4.

- [ ] **Step 1: Write the failing test**

Add to `src/wm.rs`'s tests module (find it with `find_symbol` on `tests`; use the same imports style as neighboring tests):

```rust
#[test]
fn project_opens_land_in_the_drain_and_drain_empties() {
    let ctx = egui::Context::default();
    let mut wm = WindowManager::new().as_desktop();
    wm.add_project(Shell::PowerShell, std::path::PathBuf::from("C:\\proj"), &ctx);
    wm.add_project_with_command(std::path::PathBuf::from("C:\\agent"), "claude", &ctx);
    assert_eq!(
        wm.take_opened(),
        vec![
            (std::path::PathBuf::from("C:\\proj"), None),
            (std::path::PathBuf::from("C:\\agent"), Some("claude".to_string())),
        ]
    );
    assert!(wm.take_opened().is_empty(), "take drains");
}
```

Note: `add_project` spawns a real PTY. The drain push MUST happen before the
terminal spawn (step 3) so this test stays green even if PowerShell can't
spawn in a test environment — assert only on the drain, nothing else.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test project_opens_land_in_the_drain 2>&1 | Select-Object -Last 10`
Expected: compile error — no method `take_opened`.

- [ ] **Step 3: Implement**

1. Add the field to the `WindowManager` struct (near other `Vec` state fields):

```rust
/// Project opens since the last drain: (cwd, injected command if any).
/// Pushed by add_project / add_project_with_command; the app drains it each
/// frame to record recents. The engine never learns what a "recent" is —
/// it only reports (spec: open-drain seam).
opened: Vec<(PathBuf, Option<String>)>,
```

2. Initialize `opened: Vec::new(),` in `WindowManager::new()`.

3. First line of `add_project`'s body (before `next_slot`):

```rust
self.opened.push((cwd.clone(), None));
```

4. First line of `add_project_with_command`'s body:

```rust
self.opened.push((cwd.clone(), Some(command.to_string())));
```

5. New method next to `add_project`:

```rust
/// Drain project opens recorded since the last call (most callers: the app,
/// once per frame, to feed the recents list).
pub fn take_opened(&mut self) -> Vec<(PathBuf, Option<String>)> {
    std::mem::take(&mut self.opened)
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test project_opens_land_in_the_drain 2>&1 | Select-Object -Last 5`
Expected: `1 passed`. Then `cargo test ::wm 2>&1 | Select-Object -Last 5` — no regressions.

- [ ] **Step 5: Commit**

```powershell
git add src/wm.rs
git commit -m "feat(wm): open drain reports project opens for recents recording

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: landing pure seams — layout band + focus-zone state machine

**Files:**
- Modify: `src/landing.rs` — `LandingLayout`, `layout()`, new `Zone`/`NavKey`/`step`, `SessionKind` derives + `from_kind_str`, tests module (existing `layout(` calls gain a third argument)
- Test: inline in `src/landing.rs` tests module

**Interfaces:**
- Consumes: nothing from Tasks 1–2 (pure).
- Produces (Task 4 relies on):
  - `fn layout(area: egui::Rect, n_icons: usize, n_recents: usize) -> LandingLayout` with new fields `recents_header: egui::Rect`, `recents: Vec<egui::Rect>`
  - `enum Zone { Field, Recents }` (derives `Clone, Copy, PartialEq, Eq, Debug`)
  - `enum NavKey { Tab, Up, Down, Enter, Esc, Text }` (derives `Clone, Copy, Debug`)
  - `fn step(zone: Zone, sel: usize, len: usize, key: NavKey) -> (Zone, usize, Option<usize>)`
  - `SessionKind::from_kind_str(s: &str) -> SessionKind` (and `SessionKind` derives `PartialEq, Eq, Debug` if it doesn't already)

- [ ] **Step 1: Write the failing tests**

Add to `src/landing.rs`'s tests module:

```rust
#[test]
fn recents_band_sits_below_icons_inside_area() {
    let a = area();
    let l = layout(a, 3, 4);
    assert_eq!(l.recents.len(), 4);
    let icons_bottom = l.icons.iter().map(|r| r.bottom()).fold(f32::MIN, f32::max);
    assert!(l.recents_header.top() > icons_bottom, "band below the icon row");
    assert!(a.contains_rect(l.recents_header));
    for r in &l.recents {
        assert!(a.contains_rect(*r));
    }
    for w in l.recents.windows(2) {
        assert!(w[1].top() >= w[0].bottom(), "rows don't overlap");
        assert_eq!(w[0].height(), w[1].height(), "rows equal height");
    }
}

#[test]
fn zero_recents_hides_the_band() {
    let l = layout(area(), 3, 0);
    assert!(l.recents.is_empty());
}

#[test]
fn tab_toggles_zones_and_arrows_step_clamp_and_exit_at_top() {
    assert_eq!(step(Zone::Field, 0, 3, NavKey::Tab), (Zone::Recents, 0, None));
    assert_eq!(step(Zone::Recents, 2, 3, NavKey::Tab), (Zone::Field, 0, None));
    assert_eq!(step(Zone::Recents, 0, 3, NavKey::Down), (Zone::Recents, 1, None));
    assert_eq!(step(Zone::Recents, 2, 3, NavKey::Down), (Zone::Recents, 2, None), "clamps");
    assert_eq!(step(Zone::Recents, 1, 3, NavKey::Up), (Zone::Recents, 0, None));
    assert_eq!(step(Zone::Recents, 0, 3, NavKey::Up), (Zone::Field, 0, None), "top exits");
}

#[test]
fn enter_opens_and_esc_text_empty_return_to_field() {
    assert_eq!(step(Zone::Recents, 2, 3, NavKey::Enter), (Zone::Field, 0, Some(2)));
    assert_eq!(step(Zone::Recents, 1, 3, NavKey::Esc), (Zone::Field, 0, None));
    assert_eq!(step(Zone::Recents, 1, 3, NavKey::Text), (Zone::Field, 0, None));
    assert_eq!(step(Zone::Field, 0, 0, NavKey::Tab), (Zone::Field, 0, None), "empty list inert");
}

#[test]
fn kind_strings_map_back_with_unknown_falling_to_terminal() {
    assert_eq!(SessionKind::from_kind_str("claude"), SessionKind::Claude);
    assert_eq!(SessionKind::from_kind_str("codex"), SessionKind::Codex);
    assert_eq!(SessionKind::from_kind_str("terminal"), SessionKind::Terminal);
    assert_eq!(SessionKind::from_kind_str("future-agent"), SessionKind::Terminal);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib landing 2>&1 | Select-Object -Last 10`
Expected: compile errors — `layout` arity, `Zone`, `step`, `from_kind_str` missing.

- [ ] **Step 3: Implement**

1. `LandingLayout` gains two fields:

```rust
recents_header: egui::Rect,
recents: Vec<egui::Rect>,
```

2. Replace `layout` with (existing body plus the band; constants named so the
   render task reuses them):

```rust
/// Recents band metrics (below the icon row, which paints labels ~24px under
/// the icons — BAND_GAP clears them).
const BAND_GAP: f32 = 44.0;
const HEADER_H: f32 = 18.0;
const ROW_H: f32 = 24.0;
const ROW_GAP: f32 = 4.0;

/// Place the stack (wordmark → tagline → field → icon row → recents) centered
/// in `area`. Pure arithmetic — no fonts, no fs.
fn layout(area: egui::Rect, n_icons: usize, n_recents: usize) -> LandingLayout {
    let cx = area.center().x;
    let field_w = area.width().min(520.0).max(0.0);
    let (word_h, tag_h, field_h, icon, gap) = (120.0_f32, 24.0, 26.0, 72.0_f32, 18.0);
    let recents_h = if n_recents > 0 {
        BAND_GAP + HEADER_H + 6.0 + ROW_H * n_recents as f32 + ROW_GAP * (n_recents as f32 - 1.0)
    } else {
        0.0
    };
    let total = word_h + 16.0 + tag_h + 28.0 + field_h + 36.0 + icon + recents_h;
    let mut y = area.center().y - total / 2.0;

    let centered = |w: f32, y: f32, h: f32| {
        egui::Rect::from_min_size(egui::pos2(cx - w / 2.0, y), egui::vec2(w, h))
    };
    let word_w = area.width().min(760.0);
    let wordmark = centered(word_w, y, word_h);
    y += word_h + 16.0;
    let tagline = centered(word_w, y, tag_h);
    y += tag_h + 28.0;
    let field = centered(field_w, y, field_h);
    y += field_h + 36.0;

    let n = n_icons.max(1);
    let row_w = (icon * n as f32 + gap * (n as f32 - 1.0)).min(area.width());
    let mut x = cx - row_w / 2.0;
    let icons = (0..n_icons)
        .map(|_| {
            let r = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(icon, icon));
            x += icon + gap;
            r
        })
        .collect();
    y += icon;

    let (recents_header, recents) = if n_recents > 0 {
        y += BAND_GAP;
        let header = centered(field_w, y, HEADER_H);
        y += HEADER_H + 6.0;
        let mut rows = Vec::with_capacity(n_recents);
        for _ in 0..n_recents {
            rows.push(centered(field_w, y, ROW_H));
            y += ROW_H + ROW_GAP;
        }
        (header, rows)
    } else {
        (egui::Rect::NOTHING, Vec::new())
    };

    LandingLayout {
        wordmark,
        tagline,
        field,
        icons,
        recents_header,
        recents,
    }
}
```

3. Update the ONE existing production call site (`Landing::show`:
   `let l = layout(area, ICON_ORDER.len());`) to `layout(area, ICON_ORDER.len(), 0)`
   for now (Task 4 threads the real count), and every existing test call
   `layout(a, N)` → `layout(a, N, 0)`.

4. Add the focus-zone state machine (near `LandingLayout`):

```rust
/// Which part of the landing owns navigation keys. `Field` is the picker's
/// text field (default); `Recents` is the list under the icon row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Zone {
    Field,
    Recents,
}

#[derive(Clone, Copy, Debug)]
enum NavKey {
    Tab,
    Up,
    Down,
    Enter,
    Esc,
    Text,
}

/// Pure keyboard model for field↔recents focus (spec: Tab enters, ↑/↓ step,
/// ↑ past the top / Esc / Tab / typing return to the field, Enter opens).
/// Returns (zone, selection, row index to open).
fn step(zone: Zone, sel: usize, len: usize, key: NavKey) -> (Zone, usize, Option<usize>) {
    if len == 0 {
        return (Zone::Field, 0, None);
    }
    match (zone, key) {
        (Zone::Field, NavKey::Tab) => (Zone::Recents, 0, None),
        (Zone::Field, _) => (Zone::Field, sel, None),
        (Zone::Recents, NavKey::Up) if sel > 0 => (Zone::Recents, sel - 1, None),
        (Zone::Recents, NavKey::Up) => (Zone::Field, 0, None),
        (Zone::Recents, NavKey::Down) => (Zone::Recents, (sel + 1).min(len - 1), None),
        (Zone::Recents, NavKey::Enter) => (Zone::Field, 0, Some(sel)),
        (Zone::Recents, NavKey::Tab | NavKey::Esc | NavKey::Text) => (Zone::Field, 0, None),
    }
}
```

5. On `SessionKind`: ensure the derive list includes `PartialEq, Eq, Debug`
   (add what's missing), and add to `impl SessionKind`:

```rust
/// Map a persisted recents kind string back to a kind. Unknown strings (a
/// future agent written by a newer build) degrade to Terminal — per the
/// recents spec, one bad entry must never cost the list.
pub fn from_kind_str(s: &str) -> SessionKind {
    match s {
        "claude" => SessionKind::Claude,
        "codex" => SessionKind::Codex,
        _ => SessionKind::Terminal,
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib landing 2>&1 | Select-Object -Last 10`
Expected: all landing tests pass (old suite + 5 new).

- [ ] **Step 5: Commit**

```powershell
git add src/landing.rs
git commit -m "feat(landing): pure recents band layout + Tab focus-zone state machine

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: wire it — landing render/input, picker accessor, App recording

**Files:**
- Modify: `src/dirpicker.rs` (promote `is_open` from `#[cfg(test)]` to runtime)
- Modify: `src/landing.rs` (`Landing` fields, `reopen`, `show`)
- Modify: `src/main.rs` (`App` field + init, startup drain discard, landing call site, per-frame recording)
- Test: existing suites (no new unit tests — this is the GUI seam; Task 5 is the evidence loop)

**Interfaces:**
- Consumes: `Recents`/`RecentEntry`/`MAX_RECENTS`/`kind_of_command` (Task 1), `take_opened` (Task 2), `layout`/`Zone`/`NavKey`/`step`/`from_kind_str` (Task 3).
- Produces: `Landing::show(&mut self, ui: &mut egui::Ui, area: egui::Rect, recents: &[RecentEntry]) -> Option<LandingAction>` — the new signature `main.rs` calls.

- [ ] **Step 1: Promote the picker accessor**

In `src/dirpicker.rs`, the method is currently test-gated:

```rust
#[cfg(test)]
fn is_open(&self) -> bool {
    self.open
}
```

Replace with (landing needs it at runtime to know Tab is free):

```rust
/// Whether the completion popup is showing (and therefore owns Tab/arrows).
pub fn is_open(&self) -> bool {
    self.open
}
```

- [ ] **Step 2: Landing state + show**

In `src/landing.rs`:

1. `Landing` struct and `new` gain the zone state:

```rust
pub struct Landing {
    picker: DirPicker,
    zone: Zone,
    sel: usize,
}
```

```rust
pub fn new(start: PathBuf) -> Self {
    Self {
        picker: DirPicker::new(start),
        zone: Zone::Field,
        sel: 0,
    }
}
```

2. `reopen` resets the zone:

```rust
pub fn reopen(&mut self) {
    self.picker.reopen();
    self.zone = Zone::Field;
    self.sel = 0;
}
```

3. `show` — new signature and three insertions (imports: add
   `use crate::recents::RecentEntry;` at the top of the file). Signature:

```rust
pub fn show(
    &mut self,
    ui: &mut egui::Ui,
    area: egui::Rect,
    recents: &[RecentEntry],
) -> Option<LandingAction> {
```

   **Insertion A — visible entries + layout count** (replaces the current
   first line `let l = layout(area, ICON_ORDER.len());`):

```rust
// Display-only filter: an entry whose dir is missing (unplugged drive) is
// hidden, not deleted — it comes back when the drive does (spec).
let visible: Vec<&RecentEntry> = recents.iter().filter(|e| e.path.is_dir()).collect();
let l = layout(area, ICON_ORDER.len(), visible.len());
```

   **Insertion B — keyboard, immediately after Insertion A** (must run before
   `self.picker.show` so consumed keys never reach the field's `TextEdit`;
   only claims keys the picker ignores — Tab when the popup is closed, and
   the rest only while the recents zone holds focus):

```rust
let mut action: Option<LandingAction> = None;
if !self.picker.is_open() && !visible.is_empty() {
    let nav = ui.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Tab) {
            Some(NavKey::Tab)
        } else if self.zone == Zone::Recents {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                Some(NavKey::Up)
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                Some(NavKey::Down)
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                Some(NavKey::Enter)
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                Some(NavKey::Esc)
            } else if i.events.iter().any(|e| matches!(e, egui::Event::Text(_))) {
                Some(NavKey::Text) // typing always means "edit the path"
            } else {
                None
            }
        } else {
            None
        }
    });
    if let Some(key) = nav {
        let (zone, sel, open) = step(self.zone, self.sel, visible.len(), key);
        self.zone = zone;
        self.sel = sel;
        if let Some(idx) = open {
            let e = visible[idx];
            action = Some(LandingAction {
                path: e.path.clone(),
                kind: SessionKind::from_kind_str(&e.kind),
            });
        }
    }
} else {
    self.zone = Zone::Field; // popup open or list empty: field owns keys
}
```

   (Delete the old `let mut action: Option<LandingAction> = None;` further
   down — it moved up here.)

   **Insertion C — the band, after the icon-row loop, before `action`:**

```rust
if !visible.is_empty() {
    ui.painter().text(
        l.recents_header.left_center(),
        egui::Align2::LEFT_CENTER,
        "Recent",
        egui::FontId::proportional(12.0),
        DIM,
    );
    let row_font = egui::FontId::proportional(13.0);
    for (idx, (r, e)) in l.recents.iter().zip(visible.iter()).enumerate() {
        let resp = ui.interact(*r, ui.id().with(("recent", idx)), egui::Sense::click());
        let selected = self.zone == Zone::Recents && idx == self.sel;
        if selected || resp.hovered() {
            ui.painter()
                .rect_filled(*r, egui::CornerRadius::same(3), SEL_BG);
        }
        if selected {
            ui.painter().text(
                egui::pos2(r.min.x + 6.0, r.center().y),
                egui::Align2::LEFT_CENTER,
                ">",
                egui::FontId::monospace(13.0),
                TEXT,
            );
        }
        let kind = SessionKind::from_kind_str(&e.kind);
        let tex = icons::texture(ui.ctx(), icon_of(kind), 16);
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(r.min.x + 28.0, r.center().y),
            egui::vec2(16.0, 16.0),
        );
        ui.painter().image(
            tex.id(),
            icon_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        let name = e
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| e.path.display().to_string());
        let name_x = r.min.x + 44.0;
        let name_w = ui
            .painter()
            .layout_no_wrap(name.clone(), row_font.clone(), TEXT)
            .rect
            .width();
        ui.painter().text(
            egui::pos2(name_x, r.center().y),
            egui::Align2::LEFT_CENTER,
            name,
            row_font.clone(),
            TEXT,
        );
        if let Some(parent) = e.path.parent() {
            ui.painter().text(
                egui::pos2(name_x + name_w + 10.0, r.center().y),
                egui::Align2::LEFT_CENTER,
                parent.display().to_string(),
                row_font.clone(),
                DIM,
            );
        }
        if resp.clicked() {
            action = Some(LandingAction {
                path: e.path.clone(),
                kind,
            });
        }
    }
}
```

- [ ] **Step 3: App wiring in `src/main.rs`**

1. `App` gains a field (after `notify`):

```rust
/// Recent-project MRU (recents.json), fed by the desktop's open drain.
recents: recents::Recents,
```

   and in `App::new`: `recents: recents::Recents::load(),`

2. In the `if !self.started` startup block (`main.rs:377-383`), right after
   the `if !self.landing_enabled { ... }` auto-project block, add:

```rust
// The startup auto-project is implicit (launch cwd), not a choice —
// discard its drain entry so it never pollutes recents (spec).
let _ = self.desktop.take_opened();
```

3. Landing call site (`main.rs:407`): `self.landing.show(ui, area)` →
   `self.landing.show(ui, area, self.recents.entries())` (disjoint field
   borrows — compiles as-is).

4. At the end of `App::ui`, after the desktop/landing branch and modals have
   all run (find the end of the `ui` method body), add the per-frame
   recording drain:

```rust
// Record deliberate project opens (landing, leader picker) into recents.
// CLI `foreman open` never creates projects, so it never appears here.
for (path, cmd) in self.desktop.take_opened() {
    self.recents
        .record(path, recents::kind_of_command(cmd.as_deref()));
}
```

- [ ] **Step 4: Build + full test suite**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 5
cargo test 2>&1 | Select-Object -Last 5
```

Expected: build OK (warning count unchanged from baseline), all tests pass.

- [ ] **Step 5: Commit**

```powershell
git add src/dirpicker.rs src/landing.rs src/main.rs
git commit -m "feat(landing): recent-projects list — render, Tab navigation, open recording

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: evidence loop + feature doc

**Files:**
- Read: `docs/HANDOFF.md` § 3 (screenshot procedure)
- Create: `docs/landing-recents.md` (check `docs/` first — if a landing feature doc already exists, extend it instead)

**Interfaces:** none — verification and documentation.

- [ ] **Step 1: Verify the recording path**

```powershell
Stop-Process -Name foreman -Force -ErrorAction SilentlyContinue
$env:FOREMAN_LANDING = "1"
cargo run
```

Manually (or via `foreman` CLI where possible): open a project via Enter in
the picker; close it (back to landing); confirm the list shows the entry with
the shell icon. Open a project via the [Claude] icon (if installed), land
again — entry shows the Claude icon. Then inspect the file:

```powershell
Get-Content "$env:APPDATA\foreman\recents.json"
```

Expected: entries most-recent-first with `"kind": "claude"` / `"terminal"`.

- [ ] **Step 2: Screenshot evidence**

Follow `docs/HANDOFF.md` § 3: run the exe, screenshot the window, `Read` the
PNG. Confirm: `Recent` header + rows with icon/name/dimmed-parent render under
the icon row; Tab shows the `>` marker on row 0; ↓ moves it; the stack is
still vertically centered.

- [ ] **Step 3: Flag-off regression check**

```powershell
Remove-Item Env:\FOREMAN_LANDING
cargo run
```

Expected: auto-opens the cwd project; closing the last project quits; the
auto-project does NOT appear in `recents.json`.

- [ ] **Step 4: Write the feature doc**

`docs/landing-recents.md`, grug-brain simple, covering: what it does (MRU of
deliberate project opens on the landing), why (one-keystroke reopen with the
right agent), how to use (Tab/arrows/Enter or click; kinds remembered), and
gotchas (kind is a plain string with Terminal fallback; missing dirs hidden
not deleted; startup auto-project and CLI `foreman open` never record; dedup
is case-insensitive). Key files section: `src/recents.rs`, `src/landing.rs`,
`src/wm.rs` (`take_opened`), `src/main.rs`, plus the spec path.

- [ ] **Step 5: Commit**

```powershell
git add docs/landing-recents.md
git commit -m "docs(landing): recent-projects feature doc

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```
