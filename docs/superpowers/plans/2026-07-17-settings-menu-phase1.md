# Settings Menu Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the settings-menu shell (modal, category rail, keyboard-driven panes) and wire every plain toggle/number/enum setting from the spec's phase 1 — no theme system, no keybindings-pane rewrite.

**Architecture:** A new `src/settings_menu.rs` module: a pure, unit-tested model (categories, row descriptors, a `Field` enum with a pure `adjust` function over `&mut Settings`) plus an egui view drawn as a desktop-level modal (same pattern as `dirpicker.rs` / `settings.rs`). All new fields live on the existing `Settings` struct in `src/config.rs`. Deep consumers read a per-frame `Arc<Settings>` seeded into egui context data — the exact mechanism `terminal::font_size` already uses. The App diffs the live copy back each frame and saves on the existing debounce.

**Tech Stack:** Rust, egui 0.34, serde. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-07-17-settings-menu-design.md`

## Global Constraints

- Windows, GNU toolchain (`stable-gnu`), linker w64devkit. Never MSVC.
- Before any `cargo build`/`run`: kill only repo-built foreman — `Get-Process foreman -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$PWD\target\*" } | Stop-Process -Force` (the Bash-tool hook does this automatically; PowerShell-tool invocations must do it manually). If `$env:FOREMAN` is `1`, do NOT kill — build with `cargo build --target-dir target/agent` instead.
- `#[serde(default)]` on `Settings` stays — it is the entire forward/backward compat story.
- Never hand-roll settings file I/O; go through `Settings::load`/`save` (`config.rs`).
- `send_settle_ms` must clamp to `0..=2000` on load AND in the UI (invariant: effective settle < `MAX_SETTLE_MS` (4000) < `REPLY_TIMEOUT` (5000)).
- All colors via `theme.rs` tokens; no literals in UI code.
- No control-plane wire changes anywhere in this plan (wire compat v1 untouched).
- Commit style: `type(scope): subject`, trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`. Never `git add -A`; add exact paths.
- GUI tasks verify by build + run + screenshot (script in `docs/HANDOFF.md` § 3), not by assertion.
- Line numbers cited below were verified 2026-07-17 at v0.2.9 (`d54bcea`); re-anchor with the named symbol if drifted.

**Deviations from spec phase 1 (deliberate, deferred):** custom shell command (`Shell` is a `Copy` enum — a `Custom(String)` variant ripples through every spawn site; land with a dedicated design later), `chat_history_default` (the const lives in the CLI client path in `control.rs`, which has no settings access — needs the default resolved GUI-side first), `min_tile_ratio` and `border_width` (pure-`layout.rs` and paint ripples; land with phase 3 theme work). The Settings *fields* for these are NOT added yet — YAGNI.

---

### Task 1: Settings fields + sanitize + live-seeding helpers

**Files:**
- Modify: `src/config.rs` (struct `Settings` ~line 93, `Default` impl ~line 109, tests ~line 122)

**Interfaces:**
- Produces: `Settings` fields listed below; `enum DefaultShell { PowerShell, Cmd, Sh }` with `pub fn to_shell(self) -> crate::terminal::Shell`; `Settings::sanitize(&mut self)`; `pub fn seed_live(ctx: &egui::Context, s: &Settings)` and `pub fn live(ctx: &egui::Context) -> std::sync::Arc<Settings>` in `config.rs`. Every later task consumes these exact names.

- [ ] **Step 1: Write failing serde/sanitize tests** (append to the existing `#[cfg(test)]` module in `config.rs`, mirroring the style of the tests at ~line 122):

```rust
#[test]
fn new_fields_default_when_missing_from_old_file() {
    // An old settings.json (font_size only) must load with every new field
    // at its default — the serde(default) contract.
    let s: Settings = serde_json::from_str(r#"{ "font_size": 15.0 }"#).unwrap();
    assert_eq!(s.default_shell, DefaultShell::PowerShell);
    assert_eq!(s.scrollback_lines, 10_000);
    assert_eq!(s.scroll_speed, 3.0);
    assert_eq!(s.zoom_step, 1.0);
    assert!(!s.copy_on_select);
    assert!(s.paste_warn_multiline);
    assert_eq!(s.bell_period, 1.2);
    assert_eq!(s.toast_secs, 6.0);
    assert!(!s.new_windows_float);
    assert!(!s.focus_follows_mouse);
    assert!(!s.dim_unfocused);
    assert!(s.install_skills);
    assert_eq!(s.crew_stale_secs, 300);
    assert_eq!(s.send_settle_ms, 120);
    assert!(s.restore_workspace);
    assert_eq!(s.default_project_dir, "");
    assert!(s.update_check);
}

#[test]
fn sanitize_clamps_hand_edited_values() {
    let mut s = Settings::default();
    s.send_settle_ms = 999_999; // must never approach MAX_SETTLE_MS (4000)
    s.scrollback_lines = 7;
    s.scroll_speed = 0.0;
    s.zoom_step = 100.0;
    s.bell_period = 0.0;
    s.toast_secs = 0.0;
    s.crew_stale_secs = 1;
    s.sanitize();
    assert_eq!(s.send_settle_ms, 2000);
    assert_eq!(s.scrollback_lines, 100);
    assert_eq!(s.scroll_speed, 1.0);
    assert_eq!(s.zoom_step, 5.0);
    assert_eq!(s.bell_period, 0.4);
    assert_eq!(s.toast_secs, 1.0);
    assert_eq!(s.crew_stale_secs, 30);
}

#[test]
fn default_shell_maps_to_terminal_shell() {
    use crate::terminal::Shell;
    assert_eq!(DefaultShell::PowerShell.to_shell(), Shell::PowerShell);
    assert_eq!(DefaultShell::Cmd.to_shell(), Shell::Cmd);
    assert_eq!(DefaultShell::Sh.to_shell(), Shell::Bash);
}

#[test]
fn settings_roundtrip_preserves_new_fields() {
    let mut s = Settings::default();
    s.default_shell = DefaultShell::Cmd;
    s.copy_on_select = true;
    s.default_project_dir = "H:\\claude code".into();
    let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(back.default_shell, DefaultShell::Cmd);
    assert!(back.copy_on_select);
    assert_eq!(back.default_project_dir, "H:\\claude code");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib config 2>&1 | Select-Object -Last 15`
Expected: compile error — `default_shell` / `DefaultShell` / `sanitize` not found.

- [ ] **Step 3: Implement.** In `src/config.rs`:

```rust
/// What a bare "new terminal" runs. Custom command lines are a later phase
/// (Shell is a Copy enum; a String variant ripples through every spawn site).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultShell {
    PowerShell,
    Cmd,
    Sh,
}

impl DefaultShell {
    pub fn to_shell(self) -> crate::terminal::Shell {
        match self {
            DefaultShell::PowerShell => crate::terminal::Shell::PowerShell,
            DefaultShell::Cmd => crate::terminal::Shell::Cmd,
            DefaultShell::Sh => crate::terminal::Shell::Bash,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            DefaultShell::PowerShell => "PowerShell",
            DefaultShell::Cmd => "CMD",
            DefaultShell::Sh => "SH",
        }
    }
}
```

Extend `Settings` (keep `#[serde(default)]` on the struct; add the new fields after `bell`, each with a one-line doc comment stating what it governs):

```rust
    // -- terminal --
    pub default_shell: DefaultShell,
    pub scrollback_lines: u32,
    pub scroll_speed: f32,
    pub zoom_step: f32,
    pub copy_on_select: bool,
    pub paste_warn_multiline: bool,
    // -- bell & alerts --
    pub bell_period: f32,
    pub toast_secs: f32,
    // -- window manager --
    pub new_windows_float: bool,
    pub focus_follows_mouse: bool,
    pub dim_unfocused: bool,
    // -- agents --
    pub install_skills: bool,
    pub crew_stale_secs: u32,
    pub send_settle_ms: u64,
    // -- startup --
    pub restore_workspace: bool,
    pub default_project_dir: String,
    pub update_check: bool,
```

`Default` impl additions (exact values): `default_shell: DefaultShell::PowerShell`, `scrollback_lines: 10_000`, `scroll_speed: 3.0`, `zoom_step: FONT_ZOOM_STEP` (the existing const, 1.0), `copy_on_select: false`, `paste_warn_multiline: true`, `bell_period: 1.2`, `toast_secs: 6.0`, `new_windows_float: false`, `focus_follows_mouse: false`, `dim_unfocused: false`, `install_skills: true`, `crew_stale_secs: 300`, `send_settle_ms: 120`, `restore_workspace: true`, `default_project_dir: String::new()`, `update_check: true`.

```rust
impl Settings {
    /// Clamp every numeric field to its legal range. Runs on load so a
    /// hand-edited file can't violate invariants (notably: settle must stay
    /// far below control.rs REPLY_TIMEOUT via wm.rs MAX_SETTLE_MS).
    pub fn sanitize(&mut self) {
        self.font_size = self.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        self.scrollback_lines = self.scrollback_lines.clamp(100, 1_000_000);
        self.scroll_speed = self.scroll_speed.clamp(1.0, 30.0);
        self.zoom_step = self.zoom_step.clamp(0.25, 5.0);
        self.bell_period = self.bell_period.clamp(0.4, 5.0);
        self.toast_secs = self.toast_secs.clamp(1.0, 30.0);
        self.crew_stale_secs = self.crew_stale_secs.clamp(30, 3600);
        self.send_settle_ms = self.send_settle_ms.min(2000);
    }
}
```

Call `s.sanitize()` inside `Settings::load()` before returning. Then the live-seeding pair (bottom of `config.rs`):

```rust
/// Seed the frame's settings into egui context data so deep consumers
/// (terminal.rs, wm.rs, chat_view.rs) can read them without threading a
/// parameter through every call. Same pattern as terminal::font_size.
pub fn seed_live(ctx: &eframe::egui::Context, s: &Settings) {
    let arc = std::sync::Arc::new(s.clone());
    ctx.data_mut(|d| d.insert_temp(eframe::egui::Id::new("foreman::settings"), arc));
}

/// The settings seeded this frame (defaults before the app's first seed).
pub fn live(ctx: &eframe::egui::Context) -> std::sync::Arc<Settings> {
    ctx.data_mut(|d| d.get_temp(eframe::egui::Id::new("foreman::settings")))
        .unwrap_or_else(|| std::sync::Arc::new(Settings::default()))
}
```

`Settings` needs `Clone` — add it to the derive list if absent.

- [ ] **Step 4: Run tests**

Run: `cargo test --lib config 2>&1 | Select-Object -Last 10`
Expected: all pass, including the pre-existing compat tests.

- [ ] **Step 5: Commit**

```powershell
git add src/config.rs
git commit -m "feat(config): phase-1 settings fields, sanitize clamps, live-seed helpers

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Settings menu model (pure)

**Files:**
- Create: `src/settings_menu.rs` (model half; view lands in Task 3)
- Modify: `src/main.rs` (add `mod settings_menu;` to the module list ~line 30)

**Interfaces:**
- Consumes: `Settings`, `DefaultShell` from Task 1.
- Produces: `enum Pane { Terminal, Bell, WindowManager, Keybindings, Agents, Startup }` with `Pane::ALL: [Pane; 6]`, `Pane::label(self) -> &'static str`; `enum Field` (one variant per editable setting, listed below); `enum Kind { Toggle, Stepper, Choice, Text, Action }`; `struct RowSpec { pub field: Field, pub label: &'static str, pub desc: &'static str, pub kind: Kind }`; `pub fn rows(pane: Pane) -> &'static [RowSpec]`; `enum Adjust { Toggle, Inc, Dec }`; `pub fn adjust(field: Field, a: Adjust, s: &mut Settings) -> bool` (returns whether anything changed); `pub fn display(field: Field, s: &Settings) -> String`; `struct SettingsMenu` with `pub fn new() -> Self`, nav fields `pane: Pane`, `row: usize`, `in_rail: bool`, and pure nav methods `nav_up/nav_down/nav_tab/select_pane`.

`Field` variants (exact names — Task 3+ match on these): `DefaultShellF, ScrollbackLines, ScrollSpeed, ZoomStep, CopyOnSelect, PasteWarn, BellOn, BellPeriod, ToastSecs, NewWindowsFloat, FocusFollowsMouse, DimUnfocused, InstallSkills, CrewStale, SendSettle, RestoreWorkspace, DefaultProjectDir, UpdateCheck, OpenKeybindings, CheckUpdatesNow, OpenConfigFolder`.

- [ ] **Step 1: Write failing tests** (in `settings_menu.rs`'s `#[cfg(test)]`):

```rust
use crate::config::{DefaultShell, Settings};

#[test]
fn every_pane_has_rows_and_labels() {
    for p in Pane::ALL {
        assert!(!p.label().is_empty());
        assert!(!rows(p).is_empty(), "{:?} has no rows", p);
    }
}

#[test]
fn toggle_flips_and_reports_change() {
    let mut s = Settings::default();
    assert!(adjust(Field::CopyOnSelect, Adjust::Toggle, &mut s));
    assert!(s.copy_on_select);
    assert!(adjust(Field::CopyOnSelect, Adjust::Toggle, &mut s));
    assert!(!s.copy_on_select);
}

#[test]
fn stepper_clamps_at_bounds_and_reports_no_change() {
    let mut s = Settings::default();
    s.send_settle_ms = 2000;
    assert!(!adjust(Field::SendSettle, Adjust::Inc, &mut s), "inc at max is a no-op");
    assert_eq!(s.send_settle_ms, 2000);
    s.scroll_speed = 1.0;
    assert!(!adjust(Field::ScrollSpeed, Adjust::Dec, &mut s));
    assert_eq!(s.scroll_speed, 1.0);
}

#[test]
fn shell_choice_cycles_through_all_variants() {
    let mut s = Settings::default();
    adjust(Field::DefaultShellF, Adjust::Inc, &mut s);
    assert_eq!(s.default_shell, DefaultShell::Cmd);
    adjust(Field::DefaultShellF, Adjust::Inc, &mut s);
    assert_eq!(s.default_shell, DefaultShell::Sh);
    adjust(Field::DefaultShellF, Adjust::Inc, &mut s);
    assert_eq!(s.default_shell, DefaultShell::PowerShell, "wraps");
}

#[test]
fn nav_stays_in_bounds_and_tab_switches_focus() {
    let mut m = SettingsMenu::new();
    assert!(m.in_rail);
    m.nav_tab();
    assert!(!m.in_rail);
    m.nav_up(); // row 0 → stays 0 (no wrap; matches keymap editor feel)
    assert_eq!(m.row, 0);
    let last = rows(m.pane).len() - 1;
    for _ in 0..rows(m.pane).len() + 5 { m.nav_down(); }
    assert_eq!(m.row, last, "clamps at last row");
}

#[test]
fn display_formats_units() {
    let s = Settings::default();
    assert_eq!(display(Field::SendSettle, &s), "120 ms");
    assert_eq!(display(Field::ScrollbackLines, &s), "10000");
    assert_eq!(display(Field::BellPeriod, &s), "1.2 s");
    assert_eq!(display(Field::DefaultShellF, &s), "PowerShell");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib settings_menu 2>&1 | Select-Object -Last 10`
Expected: compile errors (module empty).

- [ ] **Step 3: Implement the model.** Key parts (write in full):

```rust
//! Settings menu (phase 1): pure model half. Categories, declarative row
//! descriptors, and a pure `adjust` over &mut Settings — all unit-tested
//! without a GUI. The egui view lives in the same file below (Task 3).
//! Spec: docs/superpowers/specs/2026-07-17-settings-menu-design.md

use crate::config::{DefaultShell, Settings};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pane { Terminal, Bell, WindowManager, Keybindings, Agents, Startup }

impl Pane {
    pub const ALL: [Pane; 6] = [
        Pane::Terminal, Pane::Bell, Pane::WindowManager,
        Pane::Keybindings, Pane::Agents, Pane::Startup,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Pane::Terminal => "Terminal",
            Pane::Bell => "Bell & Alerts",
            Pane::WindowManager => "Window Manager",
            Pane::Keybindings => "Keybindings",
            Pane::Agents => "Agents",
            Pane::Startup => "Startup & Updates",
        }
    }
}
```

`rows()` returns static slices; every row from the mockup's phase-1 panes, e.g.:

```rust
pub fn rows(pane: Pane) -> &'static [RowSpec] {
    match pane {
        Pane::Terminal => &[
            RowSpec { field: Field::DefaultShellF, label: "Default shell",
                desc: "What a new pane runs; per-pane chips still override", kind: Kind::Choice },
            RowSpec { field: Field::ScrollbackLines, label: "Scrollback lines",
                desc: "History kept per pane (new terminals)", kind: Kind::Stepper },
            RowSpec { field: Field::ScrollSpeed, label: "Scroll speed",
                desc: "Lines per wheel notch", kind: Kind::Stepper },
            RowSpec { field: Field::ZoomStep, label: "Zoom step",
                desc: "Font points per Ctrl+Scroll notch", kind: Kind::Stepper },
            RowSpec { field: Field::CopyOnSelect, label: "Copy on select",
                desc: "Selection lands on the clipboard immediately", kind: Kind::Toggle },
            RowSpec { field: Field::PasteWarn, label: "Warn on multi-line paste",
                desc: "Confirm before pasting text containing newlines", kind: Kind::Toggle },
        ],
        // ... Bell: BellOn, BellPeriod, ToastSecs
        // ... WindowManager: NewWindowsFloat, FocusFollowsMouse, DimUnfocused
        // ... Keybindings: OpenKeybindings (Kind::Action,
        //       label "Edit keybindings…", desc "Leader, chords, conflicts — the full editor")
        // ... Agents: InstallSkills, CrewStale, SendSettle
        // ... Startup: RestoreWorkspace, DefaultProjectDir (Kind::Text),
        //       UpdateCheck, CheckUpdatesNow (Action), OpenConfigFolder (Action)
    }
}
```

(The `// ...` arms above MUST be written out in full — same shape as the Terminal arm, labels/descs from the mockup.)

`adjust` — steppers use these exact steps and the same bounds as `Settings::sanitize`: `ScrollbackLines` ±1000, `ScrollSpeed` ±1.0, `ZoomStep` ±0.25, `BellPeriod` ±0.1, `ToastSecs` ±1.0, `CrewStale` ±30, `SendSettle` ±20. Pattern:

```rust
pub fn adjust(field: Field, a: Adjust, s: &mut Settings) -> bool {
    fn step_f32(v: &mut f32, a: Adjust, step: f32, min: f32, max: f32) -> bool {
        let next = match a {
            Adjust::Inc => (*v + step).min(max),
            Adjust::Dec => (*v - step).max(min),
            Adjust::Toggle => *v,
        };
        let changed = (next - *v).abs() > f32::EPSILON;
        *v = next;
        changed
    }
    fn flip(v: &mut bool) -> bool { *v = !*v; true }
    match field {
        Field::CopyOnSelect => flip(&mut s.copy_on_select),
        Field::ScrollSpeed => step_f32(&mut s.scroll_speed, a, 1.0, 1.0, 30.0),
        Field::DefaultShellF => {
            let order = [DefaultShell::PowerShell, DefaultShell::Cmd, DefaultShell::Sh];
            let i = order.iter().position(|x| *x == s.default_shell).unwrap_or(0);
            let n = order.len();
            s.default_shell = match a {
                Adjust::Inc | Adjust::Toggle => order[(i + 1) % n],
                Adjust::Dec => order[(i + n - 1) % n],
            };
            true
        }
        // ... every other Field, same shapes (int steppers mirror step_f32 with
        //     u32/u64 arithmetic; Action fields return false — the view handles them)
    }
}
```

`display` formats with units exactly as the tests demand. `SettingsMenu` nav: `nav_up`/`nav_down` clamp (no wrap), `nav_tab` flips `in_rail`, `select_pane(p)` sets `pane`, resets `row` to 0.

- [ ] **Step 4: Run tests**

Run: `cargo test --lib settings_menu 2>&1 | Select-Object -Last 10`
Expected: all pass. Also `cargo build 2>&1 | Select-Object -Last 5` — clean (view not wired yet, `#[allow(dead_code)]` on the module if needed until Task 3).

- [ ] **Step 5: Commit**

```powershell
git add src/settings_menu.rs src/main.rs
git commit -m "feat(settings-menu): pure model — panes, row specs, adjust/display

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Settings menu view + wm/App wiring

**Files:**
- Modify: `src/settings_menu.rs` (view half)
- Modify: `src/wm.rs` (field `settings: Option<SettingsView>` ~line 396, `open_settings` ~line 2398, the draw/outcome site — find it via `SettingsOutcome` usages near `keymap.save()` ~line 3489)
- Modify: `src/main.rs` (seed + read-back + debounced save, next to the existing font flow at ~lines 537 and 620-643)

**Interfaces:**
- Consumes: Task 1 `seed_live`/`live`/`sanitize`; Task 2 model.
- Produces: `SettingsMenu::show(&mut self, ui: &mut egui::Ui, s: &mut Settings) -> MenuOutcome` with `pub enum MenuOutcome { Pending, Changed, OpenKeybindings, Close }`. wm owns `menu: Option<SettingsMenu>` and keeps `keymap_editor: Option<SettingsView>` (renamed from `settings`).

- [ ] **Step 1: Implement the view.** In `settings_menu.rs`, an egui modal mirroring `settings.rs`'s draw approach (read `SettingsView::show` at `src/settings.rs:161` first and copy its overlay scaffolding: dim layer, centered panel, input capture). Structure per the mockup:
  - Title band (`TITLE_BG_FOCUS`): `Settings — <pane label>`.
  - Body: left rail (fixed 190 px; rows = `Pane::ALL`, active pane gets `SEL_BG` wash + 2 px `BORDER_FOCUS` left edge) and right pane (rows from `rows(pane)`; each row: label + dim desc left, control right).
  - Controls by `Kind`: `Toggle` draws a 34×18 px box, amber (`BELL`) border+knob when on, `BORDER`/`DIM` when off; `Stepper` draws `− <display(field)> +` with clickable ends; `Choice` draws `display(field)` in a bordered chip (click or Left/Right cycles); `Text` draws the current value, Enter opens an inline `egui::TextEdit` (Enter commits, Esc cancels); `Action` draws a bordered button.
  - Footer (dim, bordered top): `↑↓ navigate · Tab rail⇄pane · Enter edit · ←→ adjust · Esc close`.
  - Keyboard: consume ALL input while open (same claim as the keymap editor). Up/Down → nav; Tab → `nav_tab`; Left/Right → `adjust(field, Dec/Inc, s)`; Enter → Toggle/Action/Text-edit; Esc → `MenuOutcome::Close`. In the rail, Up/Down move panes, Enter/Right jumps into the pane. Every successful `adjust` returns `MenuOutcome::Changed` that frame.
  - `Field::OpenKeybindings` + Enter → return `MenuOutcome::OpenKeybindings`.
  - `Field::OpenConfigFolder` + Enter → `std::process::Command::new("explorer").arg(crate::config::config_dir()).spawn().ok();` (check `config_dir()`'s exact name/visibility at `src/config.rs:24-34`; make it `pub` if needed).
  - `Field::CheckUpdatesNow` is drawn but disabled in this task (dim, no-op); Task 10 wires it.

- [ ] **Step 2: Wire the wm.** In `src/wm.rs`: rename field `settings` → `keymap_editor` (mechanical; fix all uses). Add `menu: Option<SettingsMenu>`. `open_settings()` (~2398) now sets `self.menu = Some(SettingsMenu::new())`. At the draw site, draw the menu first, then the keymap editor above it if open. Outcome handling:

```rust
if let Some(m) = &mut self.menu {
    match m.show(ui, &mut live_settings) {
        MenuOutcome::Close => self.menu = None,
        MenuOutcome::OpenKeybindings => self.keymap_editor = Some(SettingsView::new()),
        MenuOutcome::Changed => settings_dirty = true,
        MenuOutcome::Pending => {}
    }
}
```

Where `live_settings` comes from: at the top of the wm frame take `let mut live_settings = (*crate::config::live(ui.ctx())).clone();` and, when `settings_dirty`, re-seed with `crate::config::seed_live(ui.ctx(), &live_settings)` so the App's read-back sees the edit (this mirrors how font-size zoom publishes through ctx data today).

- [ ] **Step 3: Wire the App.** In `src/main.rs`: before `desktop.ui(...)` — next to the existing `terminal::set_font_size` call at ~537 — add `config::seed_live(&ctx, &self.settings);`. In the read-back block (~620), read `let live = config::live(&ctx);` and if `*live != self.settings` (derive `PartialEq` on `Settings` and `DefaultShell`), assign and arm the same debounce timer the font path uses (the `FONT_SAVE_DEBOUNCE` logic at ~639 — rename the timer comment to cover settings generally). `set_font_size`/`set_bell_enabled` stay as-is; they now read from the same struct the menu edits.

- [ ] **Step 4: Build, run, verify visually**

```powershell
Get-Process foreman -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$PWD\target\*" } | Stop-Process -Force; Start-Sleep -Milliseconds 500
cargo build 2>&1 | Select-Object -Last 5
cargo test --lib wm 2>&1 | Select-Object -Last 5
```

Expected: clean build; wm tests pass (the rename must not break them). Run the exe, press `Ctrl+B ,`, screenshot (script in `docs/HANDOFF.md` § 3), `Read` the PNG and confirm: rail with 6 categories, Terminal pane rows, footer hints. Toggle "Copy on select", close, reopen — the toggle held. Check `%APPDATA%\foreman\settings.json` contains `"copy_on_select": true` after ~1 s.

- [ ] **Step 5: Commit**

```powershell
git add src/settings_menu.rs src/wm.rs src/main.rs
git commit -m "feat(settings-menu): egui modal shell wired to wm and debounced save

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Wire default shell + scrollback

**Files:**
- Modify: `src/wm.rs` — the hardcoded `Shell::PowerShell` spawn sites at ~886, ~2375, ~3045 (leader `C` / split / menu paths). NOT the per-Project chip menu (~4174-4176, explicit user choice) and NOT `add_project` (~4672 — the project's first terminal SHOULD honor the default too: change it).
- Modify: `src/terminal.rs` — `Session::spawn_with` where `Config::default()` is passed at ~910.

**Interfaces:**
- Consumes: `config::live(ctx)`, `DefaultShell::to_shell()`.
- Produces: `Session` spawn honors `scrollback_lines`; new terminals honor `default_shell`.

- [ ] **Step 1: Default shell.** Each listed wm site has an `egui::Context` in scope (they pass `ctx` to `add_terminal`). Replace `Shell::PowerShell` with `crate::config::live(ctx).default_shell.to_shell()` (adjust `ctx` vs `&ctx` per site).

- [ ] **Step 2: Scrollback.** In `terminal.rs` ~910, replace `Config::default()` with:

```rust
Config {
    scrolling_history: crate::config::live(&ctx).scrollback_lines as usize,
    ..Config::default()
}
```

Verify the field name `scrolling_history` against alacritty_terminal 0.26's `term::Config` (check `~/.cargo` sources or docs.rs) — if it differs, use the actual name. The test-only `Term::new` at ~4860 stays on `Config::default()`.

- [ ] **Step 3: Test behaviorally.** `cargo test --lib terminal 2>&1 | Select-Object -Last 5` (PTY tests must stay green). Then build + run: open settings, set shell to CMD, `Ctrl+B C` → new pane must be CMD (screenshot: `Microsoft Windows [Version...]` banner, not a PS prompt).

- [ ] **Step 4: Commit**

```powershell
git add src/wm.rs src/terminal.rs
git commit -m "feat(terminal): default shell and scrollback come from settings

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Wire zoom step + scroll speed

**Files:**
- Modify: `src/input.rs` — `zoom_step` (~102-141) currently uses `FONT_ZOOM_STEP`; the wheel seam (`wheel_input` / `WheelAction`, ~ where `Scrollback(s)` is produced) currently uses a fixed lines-per-notch.
- Modify: `src/terminal.rs` — call sites ~1870 (`input::zoom_step`) and the wheel handling ~1875-1905.
- Test: existing `input.rs` test module.

**Interfaces:**
- Consumes: `Settings::zoom_step`, `Settings::scroll_speed`.
- Produces: `input::zoom_step(current: f32, steps: f32, step_size: f32) -> f32` (new param); wheel seam takes `lines_per_notch: f32`.

- [ ] **Step 1: Write failing tests** (in `input.rs`'s test module, alongside its existing zoom tests):

```rust
#[test]
fn zoom_step_honors_configured_step_size() {
    assert_eq!(zoom_step(13.0, 1.0, 2.0), 15.0);
    assert_eq!(zoom_step(13.0, -1.0, 0.5), 12.5);
    // still clamps to the font range
    assert_eq!(zoom_step(39.5, 1.0, 5.0), crate::config::MAX_FONT_SIZE);
}
```

Plus one test on the wheel seam asserting a notch scrolls `lines_per_notch` lines (mirror the shape of the existing wheel tests in that file — read them first; assert with `lines_per_notch: 5.0` that the produced `Scrollback` delta is 5 lines where the old code produced 3).

- [ ] **Step 2: Run to verify failure** — `cargo test --lib input 2>&1 | Select-Object -Last 10`. Expected: arity mismatch compile errors.

- [ ] **Step 3: Implement.** Add the parameters, delete the consts they replace (or keep them as the documented defaults referenced by `Settings::default`). Update `terminal.rs` call sites to pass `crate::config::live(ui.ctx()).zoom_step` and `.scroll_speed`.

- [ ] **Step 4: Run** — `cargo test --lib input 2>&1 | Select-Object -Last 5` and `cargo test --lib terminal 2>&1 | Select-Object -Last 5`. Expected: pass.

- [ ] **Step 5: Commit**

```powershell
git add src/input.rs src/terminal.rs
git commit -m "feat(input): zoom step and scroll speed are settings

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Copy-on-select + multi-line paste warning

**Files:**
- Modify: `src/terminal.rs` — selection finalize (find where mouse-release completes a selection; the selection/clipboard code is in this file — search `selection_to_string` or `copy`) and the paste path (search `Event::Paste` — CLAUDE.md notes Ctrl+V may arrive as `Event::Paste` AND as key events; both routes must funnel through one gate).
- Read first: `src/confirm.rs` (the existing confirm-dialog module) — reuse it; do not build a second modal.

**Interfaces:**
- Consumes: `Settings::copy_on_select`, `Settings::paste_warn_multiline`, `confirm.rs`'s API.
- Produces: a `pending_paste: Option<String>` on the type that owns per-Session UI state, drained when the confirm resolves.

- [ ] **Step 1: Copy on select.** At selection-finalize (mouse released with a non-empty selection), when `crate::config::live(ui.ctx()).copy_on_select`, run the same code the existing explicit-copy path (Ctrl+C / `Event::Copy` handler) runs. Extract that into a `fn copy_selection(&mut self, ui)` if it isn't one already, and call it from both places — do not duplicate the clipboard logic.

- [ ] **Step 2: Paste warning.** Route every paste entry point through one gate:

```rust
fn request_paste(&mut self, text: String, ui: &egui::Ui) {
    let warn = crate::config::live(ui.ctx()).paste_warn_multiline;
    if warn && text.contains(['\n', '\r']) {
        self.pending_paste = Some(text); // confirm dialog opened by caller/wm
    } else {
        self.feed_paste(&text); // the existing paste-injection path (bracketed paste aware)
    }
}
```

Wire the confirm using `confirm.rs`'s real API (read it; mirror an existing caller — search `confirm::` in `src/wm.rs` for the pattern). Dialog copy: title "Paste 3 lines?" (count `\n`s + 1), body shows the first line truncated to 60 chars + "…", buttons Paste / Cancel. On confirm → `feed_paste(&pending)`; on cancel → drop it.

- [ ] **Step 3: Test.** The gate is pure enough to unit test: a `fn paste_needs_warning(text: &str, warn: bool) -> bool` seam with tests (`"a\nb"` + warn → true; `"ab"` + warn → false; `"a\nb"` + !warn → false; `"a\rb"` + warn → true). Write test first, then the seam, then wire `request_paste` through it. Run `cargo test --lib terminal 2>&1 | Select-Object -Last 5`.

- [ ] **Step 4: Verify in-app.** Build + run: select text with copy-on-select enabled → paste elsewhere shows it; paste a multi-line clipboard into a pane → confirm dialog appears; Cancel injects nothing (screenshot both).

- [ ] **Step 5: Commit**

```powershell
git add src/terminal.rs src/wm.rs
git commit -m "feat(terminal): copy-on-select and multi-line paste confirm

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: Bell period + toast duration

**Files:**
- Modify: `src/theme.rs` — `bell_pulse(t)` (~66) gains a period param; `BELL_PERIOD` stays as the documented default.
- Modify: `src/notify.rs` — `TTL` const (~18) becomes a field on the notify center with a setter.
- Modify: call sites — `rg -n "bell_pulse" src/` for pulse consumers; `src/main.rs` seeds notify TTL from settings each frame (next to `set_bell_enabled` ~538).

**Interfaces:**
- Produces: `pub fn bell_pulse(t: f64, period: f64) -> egui::Color32`; notify center gains `pub fn set_ttl(&mut self, d: Duration)`.

- [ ] **Step 1: Adjust the existing test.** `theme.rs`'s `bell_pulse_breathes_within_the_bell_color` (~77) — parameterize with `BELL_PERIOD` and add one assertion that a custom period shifts the peak: `assert_eq!(bell_pulse(0.5, 2.0), bell_pulse(0.25, 1.0));` (same phase → same color). Run `cargo test --lib theme` — fails (arity).

- [ ] **Step 2: Implement.** Add the param (`BELL_PERIOD` → `period` inside the body). Every `bell_pulse(t)` call site passes `crate::config::live(...).bell_period as f64` (all pulse consumers are draw code with a ctx in reach). `notify.rs`: replace `TTL` uses with `self.ttl` (default `TTL`); `main.rs` calls `self.notify.set_ttl(Duration::from_secs_f32(self.settings.toast_secs))` once per frame before `show`.

- [ ] **Step 3: Run** — `cargo test --lib theme 2>&1 | Select-Object -Last 5` and `cargo test --lib notify 2>&1 | Select-Object -Last 5` (notify has pure push/prune tests — extend one to prune at a custom TTL). Expected: pass.

- [ ] **Step 4: Commit**

```powershell
git add src/theme.rs src/notify.rs src/main.rs src/wm.rs src/terminal.rs src/panel.rs
git commit -m "feat(bell): pulse period and toast duration are settings

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

(Adjust the add-list to the files `rg bell_pulse` actually touched.)

---

### Task 8: WM behaviors — float default, focus follows mouse, dim unfocused

**Files:**
- Modify: `src/wm.rs` (new-terminal creation path; per-frame pointer pass), `src/terminal.rs` (unfocused dim overlay).
- Test: wm unit-test module (it builds `WindowManager`s headlessly — mirror an existing `add_terminal` test like the one at ~6212).

**Interfaces:**
- Consumes: `Settings::new_windows_float`, `focus_follows_mouse`, `dim_unfocused`.

- [ ] **Step 1: Float default, test first.** In wm's test module:

```rust
#[test]
fn new_terminal_floats_when_setting_says_so() {
    let ctx = egui::Context::default();
    let mut s = crate::config::Settings::default();
    s.new_windows_float = true;
    crate::config::seed_live(&ctx, &s);
    let mut wm = test_wm(&ctx); // mirror however neighboring tests construct one
    let id = wm.add_terminal(Shell::Cmd, &ctx).expect("shell");
    assert!(!wm.is_tiled(id), "must spawn floating");
}
```

Adapt constructor/assertion names to the real test helpers in that module (read 3 neighboring tests first; `is_tiled` may be `tree contains` — use whatever the float-toggle tests assert on). Run: fails.

- [ ] **Step 2: Implement float default.** In `add_terminal`, after the window is created and tiled, when `crate::config::live(ctx).new_windows_float`, run the same code path `Command::TermFloat` uses to pop it out (call that function; don't re-implement rect math). Run the test: passes. Run all wm tests.

- [ ] **Step 3: Focus follows mouse.** In the wm frame pass where per-window responses/hover are known: if the setting is on, the pointer moved this frame (`ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO)`), no drag is in progress, and the hovered terminal isn't focused → focus it through the same function click-to-focus uses. Guard on pointer *movement* so focus doesn't snap when panes re-layout under a still cursor.

- [ ] **Step 4: Dim unfocused.** In `terminal.rs`'s draw, after the grid paints, when `!active && crate::config::live(ui.ctx()).dim_unfocused`: `ui.painter().rect_filled(rect, 0.0, egui::Color32::from_black_alpha(46));` — add the alpha as a `theme.rs` token `pub const DIM_UNFOCUSED: egui::Color32 = ...` (no literals in draw code, per constraints).

- [ ] **Step 5: Verify.** `cargo test --lib wm 2>&1 | Select-Object -Last 5` green; build + run: enable all three, confirm float-on-create, hover-focus, and dimming by screenshot.

- [ ] **Step 6: Commit**

```powershell
git add src/wm.rs src/terminal.rs src/theme.rs
git commit -m "feat(wm): float default, focus-follows-mouse, dim unfocused

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 9: Agents — skills gate, crew staleness, settle default

**Files:**
- Modify: `src/main.rs` — `skills_install::install()` call (~861).
- Modify: `src/chat.rs` — `STALE_AFTER` (~20) and the staleness helper (~24) gain a param; `src/chat_view.rs` callers pass the setting.
- Modify: `src/wm.rs` — settle default at ~1060.

- [ ] **Step 1: Skills gate.** Wrap the install call: `if settings.install_skills { skills_install::install(); }` — the App owns `settings` at that point (it runs at startup, before any frame). Note in a comment: takes effect next launch; the menu row's desc already says "on launch".

- [ ] **Step 2: Crew staleness, test first.** `chat.rs`'s staleness helper is pure (`d >= STALE_AFTER`). Change signature to take `stale_after: Duration`; keep `STALE_AFTER` as default-documenting const. Extend the existing chat tests: same duration judged stale at 300 s is live at 3600 s. Run chat tests → fail → implement → pass. `chat_view.rs` passes `Duration::from_secs(crate::config::live(...).crew_stale_secs as u64)`.

- [ ] **Step 3: Settle default.** At wm.rs ~1060: `req.settle_ms.unwrap_or(crate::config::live(&ctx).send_settle_ms)` (site has ctx; verify binding name). The existing `MAX_SETTLE_MS` cap on the line below stays — belt and suspenders with `sanitize`'s 2000 clamp. Extend the settle test at ~7872's module: with a seeded `send_settle_ms: 500`, a request without `settle_ms` uses 500 (mirror the existing settle test's harness).

- [ ] **Step 4: Run** — `cargo test --lib chat 2>&1 | Select-Object -Last 5`, `cargo test --lib wm 2>&1 | Select-Object -Last 5`. Expected: pass. No wire change: `settle_ms` request field untouched.

- [ ] **Step 5: Commit**

```powershell
git add src/main.rs src/chat.rs src/chat_view.rs src/wm.rs
git commit -m "feat(agents): skills-install gate, crew staleness, settle default settings

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 10: Startup — workspace gate, project dir, update gate + buttons

**Files:**
- Modify: `src/main.rs` — workspace restore (~476), update worker start (find where `update::spawn` / the initial check Effect is sent — read `src/update.rs` header + its spec section 3 first), notify/version.
- Modify: `src/wm.rs` or wherever `DirPicker::new(start)` gets its start path (search `DirPicker::new` — landing at `src/dirpicker.rs:630` shows the constructor; find the GUI caller).
- Modify: `src/settings_menu.rs` — enable `CheckUpdatesNow`.

- [ ] **Step 1: Workspace gate.** At ~476: `if self.settings.restore_workspace { ... }` around the snapshot load/apply. The `!restored` landing fallback must still run when the gate is off (restored stays false — verify the flow reads that way after the edit).

- [ ] **Step 2: Default project dir.** Where the GUI constructs the picker for "New Project": if `settings.default_project_dir` is non-empty AND the path exists (`std::path::Path::new(&s).is_dir()`), start there; else the current behavior. Invalid paths silently fall back — no error modal for a stale setting.

- [ ] **Step 3: Update gates.** Launch check runs only when `settings.update_check`. `CheckUpdatesNow` action: send the same Effect/Event the launch check uses through `update_fx` (exact variant names from `update.rs` — it's a pure state machine, the spec doc section 3 names them). The menu can't reach `update_fx` directly: route it like other wm→App requests — add a `check_updates_requested: bool` the App drains per frame (search main.rs for an existing drained-flag pattern and mirror it).
- Version row: display `env!("CARGO_PKG_VERSION")` in the Startup pane (static, no state).

- [ ] **Step 4: Verify.** `cargo test 2>&1 | Select-Object -Last 5` (full suite). Build + run: toggle restore off, relaunch → landing instead of restored projects; Check now with network → state advances (toast or title per current update UX).

- [ ] **Step 5: Commit**

```powershell
git add src/main.rs src/wm.rs src/settings_menu.rs src/dirpicker.rs
git commit -m "feat(startup): restore/update gates, default project dir, check-now

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 11: Docs + full verify

**Files:**
- Create: `docs/settings-menu.md`
- Modify: `docs/settings-persistence.md` (new fields note), `CLAUDE.md` (the `src/settings.rs` bullet — now "keybindings editor pane + settings shell entry", and add `src/settings_menu.rs` to the architecture list)

- [ ] **Step 1: Write `docs/settings-menu.md`** — grug-brain style per the user's global rules: what it does (one modal, six panes, every phase-1 setting listed with its default and range), why (consts → user config), how to use (`Ctrl+B ,`, keyboard map), gotchas (scrollback applies to new terminals; skills gate takes effect next launch; settle clamped to 2000 for the pipe-timeout invariant; hand-edited files are sanitized on load), and a **Key files** section: `src/settings_menu.rs`, `src/config.rs`, `src/settings.rs`, consumer list.

- [ ] **Step 2: Full gate.**

```powershell
cargo test 2>&1 | Select-Object -Last 10
cargo build --release 2>&1 | Select-Object -Last 5
```

Expected: all tests green, clean release build. Run release exe; walk every pane end-to-end; kill the app; corrupt `settings.json` with garbage; relaunch → app opens on defaults (corruption tolerance intact).

- [ ] **Step 3: Commit**

```powershell
git add docs/settings-menu.md docs/settings-persistence.md CLAUDE.md
git commit -m "docs(settings): settings-menu doc; update persistence + CLAUDE.md

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```
