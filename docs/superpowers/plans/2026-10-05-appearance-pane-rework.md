# Appearance Pane Rework Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Hand-edited theme JSON applies live and is never clobbered; every theme token is editable in-app from one table; the Appearance pane and its Theme Expert chat look like the rest of foreman's settings.

**Architecture:** `App` (main.rs) becomes the single owner of "what is on disk" for the active theme: it records the file mtime after every load/save and polls once a second, adopting a valid change and toasting an invalid one without renaming the file. The pane resyncs whenever the live theme differs from its working copy. A static `theme::TOKENS` table drives the pane, so adding a `Theme` field without a table row fails a test. The pane and chat are repainted with the hand-painted row/chip idiom from `settings_menu.rs` (46px rows, label + dim description, controls anchored right, bordered chips).

**Tech Stack:** Rust, egui 0.34.3 (hand-painted via `ui.painter()`, `ui.interact`), serde_json, existing `notify` toasts.

**Spec:** Design approved in chat (session 2026-10-05). Summary: (1) disk-owned-by-App reload + dirty-gated outgoing save + Reload chip; (2) `TOKENS` table with key/label/desc/group/alpha/get/set and a serde-keys test; (3) house-style pane: preset chip row, status line, grouped token rows with key + swatch, palette/chat-color swatch grids, preview with mock chrome, Theme Expert with bubbles, proposal cards, starter chips, Enter-to-send.

## Global Constraints

- You are running INSIDE foreman (`FOREMAN=1`). Build with `cargo build --target-dir target/agent`. Never `Stop-Process foreman`.
- Run tests with `cargo test --target-dir target/agent <filter>`.
- `#[serde(default)]` on `Theme` stays. The on-disk hex format (premultiplied `#rrggbbaa`) stays. No new dependencies.
- No `VoidListener` anywhere near a `Session` (not touched here, but the rule stands).
- Colors come from `theme::live(ctx)` tokens, never literals, in any new paint code.
- Commit messages: `type(scope): subject`, end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`. Never `git add -A`; add the files you touched.
- Docs cite files and symbols, never line numbers.
- GUI changes are verified by the user's build-screenshot skill; ask, do not claim.

## Review Focus

1. A user's editor truncates-then-writes the theme file: the poll must see a transient invalid file, keep the current theme, and NOT rename it `.corrupt-*`. Test in Task 1 (`try_load_leaves_an_invalid_file_untouched`).
2. The App's own debounced save changes the mtime: the poll must not treat it as a foreign edit that resets the pane. Covered by recording mtime after save in Task 2 and the resync rule test in Task 3 (equal themes never resync).
3. Preset switch with a clean pane: the outgoing file must not be written. Test in Task 3 (`outgoing_theme_is_only_persisted_when_dirty`).
4. A hand edit while a chat proposal is being previewed: the hero shows the proposal, terminals show the file. Acceptable; the caption says "Proposal preview". No test; noted in docs.
5. A `Theme` field added without a `TOKENS` row: the pane silently hides it. Test in Task 4 (`every_theme_color_has_exactly_one_token_row`).

---

### Task 1: Non-destructive `Theme::try_load` and `Theme::file_mtime`

**Files:**
- Modify: `src/theme.rs` (impl Theme, after `load`)
- Modify: `src/config.rs` (add `parse_json_from`)
- Test: `src/theme.rs` tests module

**Interfaces:**
- Produces: `pub fn try_load(name: &str) -> Result<Theme, String>` (built-in → `Ok(foreman_warm())`; missing file → `Err`; invalid → `Err(serde message)`, file untouched); `pub fn file_path(name: &str) -> Option<PathBuf>`; `pub fn file_mtime(name: &str) -> Option<std::time::SystemTime>` (None for built-in / missing).
- Produces in config.rs: `pub fn parse_json_from<T: DeserializeOwned>(dir: &Path, file: &str) -> Result<T, String>` — read + parse, no backup, no recovery bookkeeping.

- [ ] **Step 1: Write the failing tests** in `src/theme.rs` `mod tests` (follow the existing `user_theme_save_load_round_trips_and_builtin_is_readonly` test for how a temp themes dir is set up; reuse its helper):

```rust
#[test]
fn try_load_leaves_an_invalid_file_untouched() {
    let dir = temp_themes_dir(); // existing helper or tempfile pattern used by neighbours
    let path = dir.join("broken.json");
    std::fs::write(&path, b"{ \"bg\": \"#fff\" }").unwrap();
    let r = Theme::try_load_in(&dir, "broken");
    assert!(r.is_err(), "3-digit hex must be rejected");
    assert!(path.exists(), "invalid file must not be renamed or removed");
    assert!(std::fs::read_dir(&dir).unwrap().count() == 1, "no .corrupt backup created");
}

#[test]
fn try_load_reads_a_valid_file_and_builtin_needs_no_file() {
    let dir = temp_themes_dir();
    let mut t = Theme::foreman_warm();
    t.bg = egui::Color32::from_rgb(1, 2, 3);
    crate::config::save_json_in(&dir, "mine.json", &t).unwrap();
    assert_eq!(Theme::try_load_in(&dir, "mine").unwrap().bg, t.bg);
    assert!(Theme::try_load_in(&dir, "missing").is_err());
    assert_eq!(Theme::try_load(crate::appearance::BUILTIN).unwrap(), Theme::foreman_warm());
}
```

`try_load_in(dir, name)` is the dir-parameterised core; `try_load(name)` resolves `themes_dir()` and calls it (same split `load_json_from` already uses).

- [ ] **Step 2: Run to verify they fail**: `cargo test --target-dir target/agent theme::tests::try_load` — expect compile errors for `try_load_in`.

- [ ] **Step 3: Implement** in `src/config.rs`:

```rust
/// Parse a JSON file from `dir` WITHOUT the recovery machinery of
/// [`load_json_from`]: no backup, no rename, no protected-path bookkeeping.
/// For readers that must leave a half-written file alone (the theme poll).
pub fn parse_json_from<T: DeserializeOwned>(dir: &std::path::Path, file: &str) -> Result<T, String> {
    let path = dir.join(file);
    let text = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
}
```

and in `src/theme.rs` `impl Theme`:

```rust
pub fn file_path(name: &str) -> Option<std::path::PathBuf> {
    if Self::is_builtin(name) { return None; }
    crate::config::themes_dir().map(|d| d.join(format!("{}.json", slug(name))))
}
pub fn file_mtime(name: &str) -> Option<std::time::SystemTime> {
    std::fs::metadata(Self::file_path(name)?).ok()?.modified().ok()
}
pub fn try_load(name: &str) -> Result<Theme, String> {
    if Self::is_builtin(name) { return Ok(Self::foreman_warm()); }
    let dir = crate::config::themes_dir().ok_or_else(|| "no themes dir".to_string())?;
    Self::try_load_in(&dir, name)
}
pub(crate) fn try_load_in(dir: &std::path::Path, name: &str) -> Result<Theme, String> {
    crate::config::parse_json_from(dir, &format!("{}.json", slug(name)))
}
```

- [ ] **Step 4: Run** `cargo test --target-dir target/agent theme::tests` — all pass.
- [ ] **Step 5: Commit** `feat(theme): non-destructive try_load and file_mtime for disk reload`.

### Task 2: App-owned disk poll with toast on invalid

**Files:**
- Modify: `src/main.rs` (`App` fields, `App::new`, the theme block in `ui()` near the existing "Reload the theme when the active name changes" comment, `flush_all`)
- Test: `src/main.rs` `app_logic_tests` (pure helper)

**Interfaces:**
- Consumes: `Theme::try_load`, `Theme::file_mtime` (Task 1).
- Produces: pure `fn theme_disk_action(seen: Option<SystemTime>, now_mtime: Option<SystemTime>, last_bad: Option<SystemTime>) -> DiskAction` with `enum DiskAction { Nothing, Reload, }` — returns `Reload` only when `now_mtime` is `Some`, differs from `seen`, and differs from `last_bad` (so one invalid mtime toasts once).

- [ ] **Step 1: Failing test** in `app_logic_tests`:

```rust
#[test]
fn theme_disk_poll_reloads_once_per_foreign_mtime() {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let t0 = UNIX_EPOCH + Duration::from_secs(10);
    let t1 = UNIX_EPOCH + Duration::from_secs(20);
    assert_eq!(theme_disk_action(Some(t0), Some(t0), None), DiskAction::Nothing);
    assert_eq!(theme_disk_action(Some(t0), Some(t1), None), DiskAction::Reload);
    assert_eq!(theme_disk_action(Some(t0), Some(t1), Some(t1)), DiskAction::Nothing, "a bad mtime toasts once");
    assert_eq!(theme_disk_action(None, Some(t1), None), DiskAction::Reload, "first sighting of a file counts");
    assert_eq!(theme_disk_action(Some(t0), None, None), DiskAction::Nothing, "deleted file: keep current theme");
}
```

- [ ] **Step 2: Run** `cargo test --target-dir target/agent app_logic_tests::theme_disk` — fails to compile.
- [ ] **Step 3: Implement.** Add to `App`: `theme_file_mtime: Option<SystemTime>`, `theme_bad_mtime: Option<SystemTime>`, `theme_poll_at: Instant`. In `App::new` set `theme_file_mtime = Theme::file_mtime(&settings.theme)`. Add the pure fn + enum near `pending_preferences`. In `ui()`, right after the existing name-change reload block (which must now also set `self.theme_file_mtime = Theme::file_mtime(&self.settings.theme); self.theme_bad_mtime = None;`) and BEFORE the live read-back, add:

```rust
// Disk poll: a hand-edited theme file applies within a second. Skipped while
// a debounced save is pending so our own write is never mistaken for a
// foreign edit (the mtime is re-recorded right after we save).
if self.theme_dirty_at.is_none()
    && !crate::theme::Theme::is_builtin(&self.settings.theme)
    && self.theme_poll_at.elapsed() >= THEME_POLL_EVERY
{
    self.theme_poll_at = std::time::Instant::now();
    let now_mtime = crate::theme::Theme::file_mtime(&self.settings.theme);
    if theme_disk_action(self.theme_file_mtime, now_mtime, self.theme_bad_mtime) == DiskAction::Reload {
        match crate::theme::Theme::try_load(&self.settings.theme) {
            Ok(t) => {
                self.theme_file_mtime = now_mtime;
                self.theme_bad_mtime = None;
                if t != *self.active_theme {
                    self.active_theme = std::sync::Arc::new(t);
                    crate::theme::seed_live(&ctx, &self.active_theme);
                    self.notify.push(notify::Level::Info, format!("Reloaded theme \"{}\" from disk", self.settings.theme));
                }
            }
            Err(e) => {
                self.theme_bad_mtime = now_mtime;
                self.notify.push(notify::Level::Warning, format!("Theme file not applied: {e}"));
            }
        }
    }
    ctx.request_repaint_after(THEME_POLL_EVERY);
}
```

`const THEME_POLL_EVERY: Duration = Duration::from_secs(1);` beside `FONT_SAVE_DEBOUNCE`. After every `self.active_theme.save(...)` in `ui()` and `flush_all`, set `self.theme_file_mtime = Theme::file_mtime(&name)`. Check `notify::Level` has an `Info` variant; if not, use the existing non-warning level.

- [ ] **Step 4: Run** `cargo test --target-dir target/agent app_logic_tests` and `cargo build --target-dir target/agent`.
- [ ] **Step 5: Commit** `feat(theme): poll the active theme file and apply hand edits live`.

### Task 3: Dirty-gated outgoing save, live-diff resync, Reload outcome

**Files:**
- Modify: `src/appearance.rs` (`AppearanceView`, `Outcome`), `src/settings_menu.rs` (`draw_pane` Appearance arm)
- Test: `src/appearance.rs` tests

**Interfaces:**
- Produces: `Outcome::Reload`; `AppearanceView::needs_resync(&self, name: &str, live: &Theme) -> bool` (true when `name != active_name` OR `*live != self.working`); `pub fn persist_outgoing(is_builtin: bool, dirty: bool) -> bool` (pure).

- [ ] **Step 1: Failing tests**:

```rust
#[test]
fn resync_fires_on_name_change_or_foreign_live_theme_but_not_on_own_edit() {
    let mut v = AppearanceView::new();
    v.set_active("mine", Theme::foreman_warm());
    assert!(!v.needs_resync("mine", &Theme::foreman_warm()));
    assert!(v.needs_resync("other", &Theme::foreman_warm()), "name change");
    let mut disk = Theme::foreman_warm();
    disk.bg = egui::Color32::from_rgb(9, 9, 9);
    assert!(v.needs_resync("mine", &disk), "disk changed under us");
    v.working_mut().bg = disk.bg; // our own edit, already published
    assert!(!v.needs_resync("mine", &disk), "live == working after our edit");
}

#[test]
fn outgoing_theme_is_only_persisted_when_dirty() {
    assert!(!persist_outgoing(true, true), "built-in never written");
    assert!(!persist_outgoing(false, false), "clean user theme: leave the file alone");
    assert!(persist_outgoing(false, true));
}
```

- [ ] **Step 2: Run** `cargo test --target-dir target/agent appearance::tests` — compile failure.
- [ ] **Step 3: Implement.** In appearance.rs add the two fns and `Outcome::Reload`. In settings_menu.rs replace the resync `if` with `if self.appearance.needs_resync(&s.theme, theme) { self.appearance.set_active(&s.theme, theme.clone()); }`; in the `SelectPreset` arm replace the `is_builtin` check with `if crate::appearance::persist_outgoing(Theme::is_builtin(&s.theme), self.appearance.is_dirty())`; add arm:

```rust
crate::appearance::Outcome::Reload => match crate::theme::Theme::try_load(&s.theme) {
    Ok(t) => {
        *theme = t.clone();
        self.appearance.set_active(&s.theme, t);
        bump(outcome, MenuOutcome::Changed); // re-seeds; App adopts + records mtime after its save
    }
    Err(e) => crate::notify::queue(ui.ctx(), crate::notify::Level::Warning, format!("Theme file not applied: {e}")),
},
```

(The redesigned pane in Task 5 emits `Outcome::Reload` from its Reload chip. Until then nothing emits it; that is fine.)

- [ ] **Step 4: Run** `cargo test --target-dir target/agent appearance::tests settings_menu::tests` and build.
- [ ] **Step 5: Commit** `fix(appearance): never overwrite a clean theme on preset switch; resync on disk change`.

### Task 4: The `TOKENS` table

**Files:**
- Modify: `src/theme.rs` (new section after `impl Default for Theme`)
- Test: `src/theme.rs` tests

**Interfaces:**
- Produces:

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenGroup { Terminal, Windows, Text, AppBar, Chat, Search }
impl TokenGroup { pub const ALL: [TokenGroup; 6]; pub fn label(self) -> &'static str; }
pub struct TokenSpec {
    pub key: &'static str,      // the JSON key, e.g. "title_bg_focus"
    pub label: &'static str,    // "Title bar (focused)"
    pub desc: &'static str,     // "Title band of the focused window"
    pub group: TokenGroup,
    pub alpha: bool,            // edited with an alpha channel
    pub get: fn(&Theme) -> egui::Color32,
    pub set: fn(&mut Theme, egui::Color32),
}
pub const TOKENS: &[TokenSpec];
pub const PALETTE_NAMES: [&str; 16];   // "Black" … "Bright White" (move from appearance.rs)
pub const CHAT_COLOR_DESC: &str;       // "Member colours, assigned in join order"
```

Row order inside a group is display order. Group → keys:
- Terminal: `bg` "Background", `fg` "Text", `caret` "Cursor" (alpha), `selection` "Selection" (alpha), `scroll_thumb` "Scrollbar thumb" (alpha), `dim_unfocused` "Unfocused dim wash" (alpha).
- Windows: `desk_bg` "Desktop", `win_bg` "Window", `title_bg` "Title bar", `title_bg_focus` "Title bar (focused)", `tab_bg` "Tab", `tab_bg_hover` "Tab (hover)", `win_btn_hover` "Window button (hover)", `win_btn_danger_hover` "Close button (hover)", `border` "Border", `border_focus` "Focus border", `proj_border_focus` "Project focus border", `scrim` "Modal scrim" (alpha), `snap_fill` "Drag target fill" (alpha), `snap_stroke` "Drag target outline".
- Text: `text` "UI text", `dim` "Dim text", `sel_bg` "List row highlight" (alpha), `selection_text_bg` "Text field selection" (alpha), `danger` "Danger", `bell` "Accent / bell".
- AppBar: `chrome_bg` "Bar", `chrome_border` "Bar border", `chrome_btn_hover` "Button (hover)", `chrome_close_hover` "Close (hover)".
- Chat: `chat_live` "Live member", `chat_stale` "Stale member", `chat_edge` "Mention edge", `chat_mention_bg` "Mention background".
- Search: `search_match` "Match" (alpha), `search_current` "Current match" (alpha), `search_bar_bg` "Bar", `search_bar_border` "Bar border", `search_error` "Error".

- [ ] **Step 1: Failing test**:

```rust
#[test]
fn every_theme_color_has_exactly_one_token_row() {
    let json = serde_json::to_value(Theme::foreman_warm()).unwrap();
    let mut file_keys: Vec<&str> = json.as_object().unwrap().keys().map(|s| s.as_str())
        .filter(|k| *k != "palette" && *k != "chat_colors").collect();
    file_keys.sort();
    let mut table: Vec<&str> = TOKENS.iter().map(|t| t.key).collect();
    table.sort();
    let mut dedup = table.clone(); dedup.dedup();
    assert_eq!(dedup.len(), table.len(), "duplicate key in TOKENS");
    assert_eq!(table, file_keys, "TOKENS must list every colour field once");
}

#[test]
fn token_accessors_round_trip_and_groups_are_all_used() {
    let mut t = Theme::foreman_warm();
    for spec in TOKENS {
        let c = egui::Color32::from_rgba_premultiplied(7, 8, 9, 255);
        (spec.set)(&mut t, c);
        assert_eq!((spec.get)(&t), c, "{} get/set mismatch", spec.key);
        assert!(!spec.label.is_empty() && !spec.desc.is_empty(), "{} needs label+desc", spec.key);
    }
    for g in TokenGroup::ALL { assert!(TOKENS.iter().any(|s| s.group == g), "{g:?} has no rows"); }
}
```

- [ ] **Step 2: Run** `cargo test --target-dir target/agent theme::tests::every_theme` — compile failure.
- [ ] **Step 3: Implement** with a local macro to keep it one line per token:

```rust
macro_rules! tok {
    ($key:ident, $label:expr, $desc:expr, $group:ident, $alpha:expr) => {
        TokenSpec { key: stringify!($key), label: $label, desc: $desc, group: TokenGroup::$group, alpha: $alpha,
            get: |t| t.$key, set: |t, c| t.$key = c }
    };
}
pub const TOKENS: &[TokenSpec] = &[
    tok!(bg, "Background", "Terminal pane surface", Terminal, false),
    // … every row from the table above, in order …
];
```

- [ ] **Step 4: Run** `cargo test --target-dir target/agent theme::tests`.
- [ ] **Step 5: Commit** `feat(theme): TOKENS table — one labelled row per colour field`.

### Task 5: Redesign the Appearance pane (house style, generated from TOKENS)

**Files:**
- Modify: `src/appearance.rs` (replace `show`, `draw_form`, `draw_hero`, `paint_preview`, `opaque_row`, `translucent_row`, `palette_grid`)
- Modify: `src/theme_expert.rs` (`Turn` gains `proposal: Option<usize>`)
- Reference idioms: `src/settings_menu.rs` `draw_pane` row loop + `draw_control` (`Kind::Choice` chip, `Kind::Text` inline edit, `Kind::Stepper`, `Kind::Action` chip); `src/chat_view.rs` bubbles.
- Test: `src/appearance.rs`, `src/theme_expert.rs`

**Interfaces:**
- Consumes: `theme::TOKENS`, `TokenGroup`, `PALETTE_NAMES`, `Outcome::Reload`, `needs_resync`.
- Produces: `Outcome::OpenThemesFolder` (settings_menu arm calls the same opener `Field::OpenConfigFolder` uses, pointing at `themes_dir()`).

Layout (all painted with `theme::live` tokens; `pad = 18.0`, `row_h = 46.0` for token rows, section header 28px with 11pt bold dim label like `"TERMINAL"`):

1. **Top strip** (two bands, full pane width):
   - Band 1: preset chips left-to-right, one per `presets` entry, `Kind::Choice`-style bordered chip; the active one uses `border_focus` and `title_bg_focus` fill; click → `SelectPreset`. Right-anchored action chips: `Duplicate`, `Reload`, `Delete` (user themes only, opens the existing confirm modal), `Folder`.
   - Band 2 (status line, 11pt dim): built-in → `Built-in · edits save as a copy`; user → `themes\<slug>.json · auto-saves`. When `is_dirty()` append ` · ` and a `Revert` chip. User theme name is editable with the `Kind::Text` inline idiom at the left of band 2 (click the name → TextEdit, Enter commits → `Outcome::Rename`, Esc cancels).
2. **Body** split as today (stacked when tall/narrow; same rule). **Left column** is a vertical `ScrollArea` (`id_salt "appearance_tokens"`):
   - A `Font size` stepper row first (same stepper paint as `Kind::Stepper`, writes `terminal::set_font_size`), under a `TERMINAL` header.
   - Then for each `TokenGroup::ALL`: header, then one row per `TOKENS` entry in that group: label 13pt `text` at `x+pad`, `desc` 11pt `dim` beneath; right side: the key in 11pt `dim` monospace (`egui::FontId::monospace(11.0)`), then a 44×20 swatch chip painted with the current colour (alpha tokens over a 2-tone checker painted with `win_bg`/`title_bg`) and a 1px `border` stroke (`border_focus` on hover). The chip is the click target for the egui colour picker: `ui.put(chip_rect, |ui| color_edit_button_srgba(...))` is NOT used; instead keep egui's picker by calling `ui.allocate_new_ui(UiBuilder::new().max_rect(chip_rect), |ui| { ui.spacing_mut().interact_size = chip_rect.size(); if spec.alpha { ui.color_edit_button_srgba_unmultiplied(..) } else { ui.color_edit_button_srgb(..) } })` and then painting the border on top so the stock button is hidden under our chip. Hover text: `format!("{} · {}\n{}", spec.key, color_hex::to_hex(c), spec.desc)`.
   - `PALETTE` header + description "ANSI colours 0–15 used by programs in the terminal" + two 8-swatch rows (`Base`, `Bright`) using `PALETTE_NAMES` for hover; same chip paint, 28×18.
   - `CHAT` group rows, then `Member colours` 6-swatch row with `CHAT_COLOR_DESC`.
   - Every change sets `out.changed`.
3. **Right column, preview** (`draw_hero`): a mock window: title band (`title_bg_focus`) with two tab chips (`tab_bg` active / `tab_bg_hover` inactive), a close glyph that paints `win_btn_danger_hover` as a small square; body `bg` with the existing sample lines, selection wash, caret; outer 1px `border_focus`. Caption unchanged ("Live preview…" / "Proposal preview · Discard" where Discard is a chip that clears `expert.selected`).
4. **Right column, Theme Expert** (`draw_expert`):
   - Header: `THEME EXPERT` section header; right-anchored provider chip (click cycles Codex→Claude→Grok, clears `model`) and model as dim click-to-edit text (`(provider default)` when empty).
   - Log: `ScrollArea` stick-to-bottom. Each `Turn`: user → right-aligned bubble filled `sel_bg`, max 78% width; expert → left-aligned bubble filled `win_bg` with 1px `border`. Text wrapped via `painter.layout(text, FontId::proportional(12.5), colour, max_w)`. Under an expert turn whose `proposal` is `Some(i)`: a card (fill `title_bg`, 1px `border`, `border_focus` when `expert.selected == Some(i)`): 10 mini swatches (bg, fg, palette[1..=8]) 14×14, then chips `Preview`/`Previewing` (toggle `expert.selected`) and `Apply` (sets `apply = true` after selecting `i`).
   - Empty state: dim line "Describe colours, contrast or a mood." plus three chips `Warmer, less orange`, `High-contrast dark`, `Soft pastel light` that set `expert.input`.
   - Busy: dim `Thinking` + 1–3 dots from `(ui.input(|i| i.time) * 2.0) as usize % 3`; request repaint while busy.
   - Error: one line in `danger` under the log; full text on hover.
   - Input: `TextEdit::multiline` 2 rows, `Send` chip right. Enter (no Shift) sends: check `ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift)` while the field has focus, then strip the trailing newline egui inserted before `send`.
5. `show()` return mapping stays; add `Reload` and `OpenThemesFolder` from the chips.

- [ ] **Step 1: Failing test** in `src/theme_expert.rs`:

```rust
#[test]
fn expert_replies_link_to_their_proposal() {
    let mut x = ThemeExpert::new();
    x.turns.push(Turn { user: true, text: "warmer".into(), proposal: None });
    x.accept(Reply { message: "done".into(), theme: Theme::foreman_warm() });
    assert_eq!(x.proposals.len(), 1);
    assert_eq!(x.turns.last().unwrap().proposal, Some(0));
    assert_eq!(x.selected, Some(0));
}
```

Refactor `poll`'s `Ok(Ok(reply))` arm into `pub fn accept(&mut self, reply: Reply)` so it is testable without a thread.

- [ ] **Step 2: Run** `cargo test --target-dir target/agent theme_expert` — compile failure.
- [ ] **Step 3: Implement** the model change + `accept`, then the pane per the layout above. Delete `opaque_row`, `translucent_row`, `palette_grid`, `CONTROLS_H` if unused. Keep `AppearanceView` public API (`set_active`, `working`, `is_dirty`, `revert`, `active_name`, `active_is_builtin`) intact — settings_menu depends on it.
- [ ] **Step 4: Run** `cargo test --target-dir target/agent appearance theme_expert settings_menu theme` and `cargo build --target-dir target/agent`; fix every warning you introduced (`cargo build` warning baseline is documented in foreman-build-and-env).
- [ ] **Step 5: Commit** `feat(appearance): house-style pane generated from TOKENS; theme expert as a chat`.

### Task 6: Docs

**Files:**
- Modify: `docs/theme-system.md`, `docs/settings-menu.md`

- [ ] **Step 1:** In `theme-system.md`: replace the "Window chrome tokens are file-only" gotcha with the TOKENS table story; add a "Hand-editing the file" section (poll cadence, toast on invalid, file never renamed by the poll, our save wins inside the debounce window, premultiplied alpha caveat); update the Appearance "How to use it" bullets and the Theme expert bullets (cards, Preview/Apply, Enter to send); add `TOKENS`/`try_load`/`file_mtime` to Key files.
- [ ] **Step 2:** In `settings-menu.md`: update the Appearance row of the pane table and the Appearance gotcha bullet.
- [ ] **Step 3: Commit** `docs(theme): hand-edit reload, TOKENS table, redesigned pane`.

### Task 7: Screenshot gate

- [ ] Ask the user to run **build-screenshot** on the Appearance pane (wide and narrow) and on the Theme Expert after one proposal. Fix what the screenshot shows. Do not claim done before this.
