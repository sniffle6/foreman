# Settings Phase 3 — Theme System + Appearance Pane + User Themes — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Convert `theme.rs`'s static color consts into a runtime `Theme` struct read through a `theme::live(ctx)` ctx seam, add a split-preview **Appearance** settings pane that edits the live theme, and persist named **user themes** to `%APPDATA%\foreman\themes\`.

**Architecture:** A `Theme` struct of `egui::Color32` fields (all ~40 tokens) is published each frame into egui ctx data by `App` (`main.rs`, beside `config::seed_live`), exactly like `Settings`. Consumers read `theme::live(ctx).<field>`. The color-resolution pipeline in `terminal.rs` (`resolve`/`glyph_style`/`indexed_rgb`/`query_color`) has no ctx (some runs off the egui thread), so it is parameterized by a plain `GridColors { fg, bg, palette }` value that render paths build from the live theme and headless/off-thread paths build from the default. Because `Theme::default() == foreman_warm() ==` today's const values, the migration renders byte-identically. The Appearance pane (a custom-body pane mirroring Keybindings) edits a working `Theme`; `App` reads it back and debounce-saves to a per-name JSON file.

**Tech Stack:** Rust, egui/eframe 0.34.3, serde/serde_json, `portable-pty`/`alacritty_terminal` (unaffected). GNU toolchain on Windows.

## Global Constraints

- **Build/test INSIDE foreman** (`$env:FOREMAN=1`): NEVER `Stop-Process foreman` (kills the host). Build/test with `--target-dir target/agent` on **every** cargo command. Bin-only crate: `cargo test <filter> --target-dir target/agent` — **never `--lib`**.
- **Byte-identity is the Stage-A/B acceptance gate:** with the default theme the app must render pixel-identical to `main` @ `0939487`. Prove with before/after second-instance screenshots.
- **TDD:** write the failing test first, watch it fail, implement minimally, watch it pass, commit. One logical change per commit.
- **Commit trailer (verbatim):** `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`. Commit subject form `type(scope): subject`. Stage exact paths — **never `git add -A`**. Commit only the files a task touches.
- **Branch:** `feat/settings-appearance-theme` (already created off `main` @ `0939487`; spec committed at `caaf0fa`).
- **Expected warning baseline ~22; suite ~708 passing + 14 ignored + ~1 flaky** (PTY/chat/DSR — re-run a lone red once before believing it).
- **Token → field naming:** snake_case of the const (`BG`→`bg`, `TITLE_BG_FOCUS`→`title_bg_focus`, `PALETTE`→`palette`, `CHAT_COLORS`→`chat_colors`). `PALETTE[i]`→`th.palette[i]`, `CHAT_COLORS[i]`→`th.chat_colors[i]`.
- **Stays a const / not a Theme field:** `BELL_PERIOD` (a `bell_period` *setting* default, read at `Settings::default()` time before any ctx — `config.rs:189`), the `unmultiplied` const helper, and `APP_BORDER` (becomes a `Theme::app_border()` method returning `self.chrome_bg`).

---

## Stage A — `Theme` struct + seam (zero behavior change)

### Task 1: `Theme` struct, `foreman_warm()` default, hex serde

**Files:**
- Modify: `src/theme.rs` (add struct + impls below the existing consts; keep the consts — they define the default)
- Test: `src/theme.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `pub struct Theme` with a `pub` `Color32` field per color token (snake_case) + `pub palette: [Color32; 16]` + `pub chat_colors: [Color32; 6]`; `impl Theme { pub fn foreman_warm() -> Self; pub fn app_border(&self) -> Color32 }`; `impl Default for Theme`; `#[derive(Clone, PartialEq)]`; serde `Serialize`/`Deserialize` with each `Color32` field via a hex codec module `color_hex`.

- [ ] **Step 1: Write the failing test** (byte-identity of the default + hex round-trip incl. the raw-premultiplied `SNAP_FILL`)

```rust
#[test]
fn foreman_warm_equals_the_legacy_consts() {
    let t = Theme::foreman_warm();
    assert_eq!(t.bg, BG);
    assert_eq!(t.text, TEXT);
    assert_eq!(t.fg, FG);
    assert_eq!(t.selection, SELECTION);
    assert_eq!(t.caret, CARET);
    assert_eq!(t.palette, PALETTE);
    assert_eq!(t.chat_colors, CHAT_COLORS);
    assert_eq!(t.snap_fill, SNAP_FILL);
    assert_eq!(t.app_border(), CHROME_BG); // APP_BORDER derivation preserved
    assert_eq!(Theme::default(), Theme::foreman_warm());
}

#[test]
fn color_hex_round_trips_opaque_and_premultiplied() {
    // Opaque → #rrggbb; premultiplied-with-alpha → #rrggbbaa; both exact.
    for c in [BG, PALETTE[3], SNAP_FILL, SELECTION, CARET] {
        let s = color_hex::to_hex(c);
        assert_eq!(color_hex::from_hex(&s).unwrap(), c, "round-trip {s}");
    }
    assert_eq!(color_hex::to_hex(BG), "#14120f");
    assert!(color_hex::from_hex("#zzz").is_err());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --target-dir target/agent theme:: 2>&1 | Select-Object -Last 20`
Expected: FAIL — `Theme`, `color_hex`, `foreman_warm` not defined.

- [ ] **Step 3: Implement the struct, `foreman_warm`, hex codec**

Add to `src/theme.rs` (below the consts). Every field is `Color32`; `foreman_warm()` reads the existing consts (single source of truth, no duplicated literals):

```rust
use serde::{Deserialize, Serialize};

/// Runtime theme: every color token as a field. `foreman_warm()` is the
/// built-in default, built from the module consts, so a default `Theme`
/// renders byte-identically to the historical static palette.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    #[serde(with = "color_hex")] pub bg: egui::Color32,
    #[serde(with = "color_hex")] pub desk_bg: egui::Color32,
    #[serde(with = "color_hex")] pub win_bg: egui::Color32,
    #[serde(with = "color_hex")] pub title_bg: egui::Color32,
    #[serde(with = "color_hex")] pub title_bg_focus: egui::Color32,
    #[serde(with = "color_hex")] pub text: egui::Color32,
    #[serde(with = "color_hex")] pub dim: egui::Color32,
    #[serde(with = "color_hex")] pub fg: egui::Color32,
    #[serde(with = "color_hex")] pub border: egui::Color32,
    #[serde(with = "color_hex")] pub border_focus: egui::Color32,
    #[serde(with = "color_hex")] pub proj_border_focus: egui::Color32,
    #[serde(with = "color_hex")] pub selection: egui::Color32,
    #[serde(with = "color_hex")] pub selection_text_bg: egui::Color32,
    #[serde(with = "color_hex")] pub sel_bg: egui::Color32,
    #[serde(with = "color_hex")] pub caret: egui::Color32,
    #[serde(with = "color_hex")] pub dim_unfocused: egui::Color32,
    #[serde(with = "color_hex")] pub bell: egui::Color32,
    #[serde(with = "color_hex")] pub scroll_thumb: egui::Color32,
    #[serde(with = "color_hex")] pub search_match: egui::Color32,
    #[serde(with = "color_hex")] pub search_current: egui::Color32,
    #[serde(with = "color_hex")] pub search_bar_bg: egui::Color32,
    #[serde(with = "color_hex")] pub search_bar_border: egui::Color32,
    #[serde(with = "color_hex")] pub search_error: egui::Color32,
    #[serde(with = "color_hex")] pub chrome_bg: egui::Color32,
    #[serde(with = "color_hex")] pub chrome_border: egui::Color32,
    #[serde(with = "color_hex")] pub chrome_btn_hover: egui::Color32,
    #[serde(with = "color_hex")] pub chrome_close_hover: egui::Color32,
    #[serde(with = "color_hex")] pub danger: egui::Color32,
    #[serde(with = "color_hex")] pub snap_fill: egui::Color32,
    #[serde(with = "color_hex")] pub snap_stroke: egui::Color32,
    #[serde(with = "color_hex")] pub chat_stale: egui::Color32,
    #[serde(with = "color_hex")] pub chat_live: egui::Color32,
    #[serde(with = "color_hex")] pub chat_edge: egui::Color32,
    #[serde(with = "color_hex")] pub chat_mention_bg: egui::Color32,
    #[serde(with = "color_hex_array")] pub chat_colors: [egui::Color32; 6],
    #[serde(with = "color_hex_array")] pub palette: [egui::Color32; 16],
}

impl Theme {
    pub fn foreman_warm() -> Self {
        Self {
            bg: BG, desk_bg: DESK_BG, win_bg: WIN_BG, title_bg: TITLE_BG,
            title_bg_focus: TITLE_BG_FOCUS, text: TEXT, dim: DIM, fg: FG,
            border: BORDER, border_focus: BORDER_FOCUS, proj_border_focus: PROJ_BORDER_FOCUS,
            selection: SELECTION, selection_text_bg: SELECTION_TEXT_BG, sel_bg: SEL_BG,
            caret: CARET, dim_unfocused: DIM_UNFOCUSED, bell: BELL, scroll_thumb: SCROLL_THUMB,
            search_match: SEARCH_MATCH, search_current: SEARCH_CURRENT,
            search_bar_bg: SEARCH_BAR_BG, search_bar_border: SEARCH_BAR_BORDER,
            search_error: SEARCH_ERROR, chrome_bg: CHROME_BG, chrome_border: CHROME_BORDER,
            chrome_btn_hover: CHROME_BTN_HOVER, chrome_close_hover: CHROME_CLOSE_HOVER,
            danger: DANGER, snap_fill: SNAP_FILL, snap_stroke: SNAP_STROKE,
            chat_stale: CHAT_STALE, chat_live: CHAT_LIVE, chat_edge: CHAT_EDGE,
            chat_mention_bg: CHAT_MENTION_BG, chat_colors: CHAT_COLORS, palette: PALETTE,
        }
    }
    /// The app frame matches the revealed OS bar — derived, never stored.
    pub fn app_border(&self) -> egui::Color32 { self.chrome_bg }
}

impl Default for Theme {
    fn default() -> Self { Self::foreman_warm() }
}

/// Serde codec: `Color32` <-> `#rrggbb` (opaque) / `#rrggbbaa` (with alpha).
/// Encodes the stored *premultiplied* bytes verbatim so every token — including
/// the raw-premultiplied `SNAP_FILL` that has no straight-alpha form — round-trips
/// exactly. `from_hex` is tolerant: bad input is a serde error (→ `serde(default)`
/// falls back to the built-in value per field).
pub mod color_hex {
    use super::egui::Color32;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn to_hex(c: Color32) -> String {
        let [r, g, b, a] = c.to_array();
        if a == 255 { format!("#{r:02x}{g:02x}{b:02x}") } else { format!("#{r:02x}{g:02x}{b:02x}{a:02x}") }
    }
    pub fn from_hex(s: &str) -> Result<Color32, String> {
        let h = s.strip_prefix('#').ok_or("missing #")?;
        let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map_err(|e| e.to_string());
        match h.len() {
            6 => Ok(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?)),
            8 => Ok(Color32::from_rgba_premultiplied(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
            _ => Err(format!("bad hex len: {s}")),
        }
    }
    pub fn serialize<S: Serializer>(c: &Color32, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&to_hex(*c))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Color32, D::Error> {
        let s = String::deserialize(d)?;
        from_hex(&s).map_err(serde::de::Error::custom)
    }
}

/// Array flavor of [`color_hex`] for the palette / chat-color arrays.
pub mod color_hex_array {
    use super::egui::Color32;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer, const N: usize>(a: &[Color32; N], s: S) -> Result<S::Ok, S::Error> {
        let v: Vec<String> = a.iter().map(|c| super::color_hex::to_hex(*c)).collect();
        v.serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(d: D) -> Result<[Color32; N], D::Error> {
        let v = Vec::<String>::deserialize(d)?;
        let mut out = [Color32::BLACK; N];
        for (i, slot) in out.iter_mut().enumerate() {
            let s = v.get(i).ok_or_else(|| serde::de::Error::custom("short color array"))?;
            *slot = super::color_hex::from_hex(s).map_err(serde::de::Error::custom)?;
        }
        Ok(out)
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --target-dir target/agent theme:: 2>&1 | Select-Object -Last 20`
Expected: PASS (all theme tests incl. the two new ones).

- [ ] **Step 5: Commit**

```bash
git add src/theme.rs
git commit -m "feat(theme): runtime Theme struct + foreman_warm default + hex serde"  # + trailer
```

### Task 2: `theme::seed_live` / `theme::live` ctx seam

**Files:**
- Modify: `src/theme.rs`
- Test: `src/theme.rs` tests

**Interfaces:**
- Produces: `pub fn seed_live(ctx: &egui::Context, t: &Theme)`, `pub fn live(ctx: &egui::Context) -> std::sync::Arc<Theme>` — byte-for-byte the shape of `config::seed_live`/`live` (`src/config.rs:235-243`), Id `"foreman::theme"`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn seed_live_round_trips_the_theme() {
    let ctx = egui::Context::default();
    let mut t = Theme::foreman_warm();
    t.bg = egui::Color32::from_rgb(1, 2, 3);
    seed_live(&ctx, &t);
    assert_eq!(*live(&ctx), t);
}

#[test]
fn live_without_seed_is_the_default() {
    let ctx = egui::Context::default();
    assert_eq!(*live(&ctx), Theme::default());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --target-dir target/agent theme::tests::seed_live 2>&1 | Select-Object -Last 15` → FAIL (unresolved `seed_live`).

- [ ] **Step 3: Implement (mirror `config::seed_live`/`live`)**

```rust
pub fn seed_live(ctx: &egui::Context, t: &Theme) {
    let arc = std::sync::Arc::new(t.clone());
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("foreman::theme"), arc));
}
pub fn live(ctx: &egui::Context) -> std::sync::Arc<Theme> {
    ctx.data_mut(|d| d.get_temp(egui::Id::new("foreman::theme")))
        .unwrap_or_else(|| std::sync::Arc::new(Theme::default()))
}
```

- [ ] **Step 4: Run to verify it passes** — `cargo test --target-dir target/agent theme:: 2>&1 | Select-Object -Last 15` → PASS.

- [ ] **Step 5: Commit**

```bash
git add src/theme.rs
git commit -m "feat(theme): seed_live/live ctx seam mirroring config"  # + trailer
```

---

## Stage B — migrate consumers to the seam (byte-identical)

> **Migration pattern for every clean consumer fn:** add `let th = crate::theme::live(ui.ctx());` (or `mui.ctx()` inside an `Area::show`/`Window::show` closure — fetch at fn top so the closure captures it) as the first line of the enclosing fn, then replace each bare `TOKEN` with `th.<field>`. Grep **case-sensitively** for each token; the SCREAMING_CASE names are distinctive. **Known false positives to skip:** `Flags::DIM` (bitflags, terminal.rs) and `IconKind::…tint()` branches (panel.rs) are not theme tokens; comment mentions of tokens are not code.
> **Completeness gate for the whole stage (Task 10):** after all consumers migrate, make the color consts private to `theme.rs`; the compiler then flags any missed bare use.

### Task 3: `bell_pulse` gains a `bell` color parameter

**Files:**
- Modify: `src/theme.rs` (signature + its 2 tests), `src/panel.rs` (4 sites), `src/wm.rs` (3 sites)

**Interfaces:**
- Consumes: nothing new. Produces: `pub fn bell_pulse(t: f64, period: f64, bell: egui::Color32) -> egui::Color32`.

- [ ] **Step 1: Update the theme.rs signature + body**: change `pub fn bell_pulse(t: f64, period: f64)` to take `bell: egui::Color32` and use `bell` in place of the `BELL` const. Update the two existing tests (`bell_pulse(BELL_PERIOD/4.0, BELL_PERIOD)` → `bell_pulse(BELL_PERIOD/4.0, BELL_PERIOD, BELL)`; the peak assertion stays `assert_eq!(peak, BELL, …)`).

- [ ] **Step 2: Run the theme tests to verify they compile+pass** — `cargo test --target-dir target/agent theme::tests::bell 2>&1 | Select-Object -Last 15` → PASS.

- [ ] **Step 3: Update the 7 call sites** to pass the live bell color. Each site already fetches `crate::config::live(ui.ctx()).bell_period`; add a `let th = crate::theme::live(ui.ctx());` at the enclosing-fn top (these fns are migrated fully in Tasks 4/6 anyway) and append `th.bell`:
  - `panel.rs:407, 671, 771, 960` — inside `paint_rail`/`paint_strip`/`paint_rail_h`/`paint_row`.
  - `wm.rs:3592, 3933, 4377` — inside `WindowManager::show`.
  Pattern per site: `bell_pulse(ui.input(|i| i.time), crate::config::live(ui.ctx()).bell_period as f64, th.bell)`.

- [ ] **Step 4: Build** — `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 20` → clean (warnings within baseline).

- [ ] **Step 5: Commit**

```bash
git add src/theme.rs src/panel.rs src/wm.rs
git commit -m "refactor(theme): bell_pulse takes the bell color as a param"  # + trailer
```

### Task 4: Migrate `wm.rs` consumers (48 refs, 5 fns)

**Files:** Modify `src/wm.rs`.

- [ ] **Step 1:** In each of `WindowManager::show`, `paint_drag_overlays`, `paint_armed_pill`, `paint_help`, and the free fn `hover_menu`, add `let th = crate::theme::live(ui.ctx());` at the top. For `hover_menu`, fetch it before the `egui::Area::new(..).show(ui.ctx(), |mui| {…})` closure (lines ~5147/5151/5161/5168 are inside it) so the closure captures `th`.
- [ ] **Step 2:** Replace bare tokens with `th.<field>` at these lines (from the anchor map): `3461 desk_bg, 3560/3806/3955 bg, 3646/3856/3870/3901 text, 3888 win_bg, 3892 border_focus, 3895 selection_text_bg, 3976 border_focus|border, 4001/4140/4175/4215/4282/4323 text|dim, 4383 proj_border_focus, 4385 border_focus, 4388 border, 4764/4780 snap_fill, 4768/4784 snap_stroke, 4928 border_focus, 5001 win_bg, 5005/5015/5036 border_focus, 5027 text, 5043 dim, 5121 text, 5147 title_bg, 5151 border, 5161 title_bg_focus, 5168 text`. (The `bell_pulse` sites were handled in Task 3.) Leave the hardcoded `egui::Color32::from_rgb(...)` chrome literals (3957/3959/4163/4203/4205/4315/4934/…) untouched — they are not theme tokens and are out of scope for colors-first.
- [ ] **Step 3: Build** — `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 20` → clean.
- [ ] **Step 4: Verify no bare tokens remain in wm.rs** — `grep -nE '\b(BG|DESK_BG|WIN_BG|TITLE_BG|TITLE_BG_FOCUS|TEXT|DIM|BORDER|BORDER_FOCUS|PROJ_BORDER_FOCUS|SELECTION_TEXT_BG|SNAP_FILL|SNAP_STROKE)\b' src/wm.rs` → only comments, if any.
- [ ] **Step 5: Commit** — `git add src/wm.rs && git commit -m "refactor(wm): read colors via theme::live seam"`  (+ trailer)

### Task 5: Migrate `chat_view.rs` (`show` clean + `chat_color` helper param)

**Files:** Modify `src/chat_view.rs`.

- [ ] **Step 1: Change the free helper's signature** so it no longer reads the glob const:

```rust
// was: fn chat_color(id: &str) -> egui::Color32 { ... CHAT_COLORS ... }
fn chat_color(id: &str, colors: &[egui::Color32; 6]) -> egui::Color32 {
    // body: CHAT_COLORS[0] -> colors[0];  CHAT_COLORS[.. % CHAT_COLORS.len()] -> colors[.. % colors.len()]
}
```

- [ ] **Step 2:** In `ChatView::show`, add `let th = crate::theme::live(ui.ctx());` at the top (ctx already used at line 89). Replace the direct token refs (`WIN_BG/DESK_BG/TITLE_BG/BORDER/TEXT/DIM/SELECTION_TEXT_BG/CHAT_STALE/CHAT_LIVE/CHAT_EDGE/CHAT_MENTION_BG` at the mapped lines) with `th.<field>`. `CHAT_COLORS[0]` at line 213 → `th.chat_colors[0]`.
- [ ] **Step 3:** Update the two `chat_color` callers (lines 105, 199) to pass the palette: `chat_color(&r.id, &th.chat_colors)` / `chat_color(id, &th.chat_colors)`.
- [ ] **Step 4: Build** — `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 20` → clean.
- [ ] **Step 5: Commit** — `git add src/chat_view.rs && git commit -m "refactor(chat): read colors via theme::live; chat_color takes the palette"` (+ trailer)

### Task 6: Migrate `settings.rs` + `panel.rs` + `confirm.rs` (68 refs, clean)

**Files:** Modify `src/settings.rs`, `src/panel.rs`, `src/confirm.rs`.

- [ ] **Step 1:** In every consuming fn (`confirm.rs::show`; `panel.rs::{show, paint_update_chip, paint_rail_update_glyph, paint_rail, paint_columns, paint_strip, paint_rail_h, paint_row}`; `settings.rs::{show, render_rows}`) add `let th = crate::theme::live(ui.ctx());` at the top. Placement caveats: `settings.rs::show` shadows `ui` with a child at line 252 — fetch before line 251 (covers the outer `WIN_BG` and the child refs). `confirm.rs::show`'s 8 refs are all inside the `egui::Window::new(..).show(ui.ctx(), |ui| {…})` closure — fetch at the top of `show` before the closure.
- [ ] **Step 2:** Replace bare tokens with `th.<field>` at the mapped lines. `BORDER.gamma_multiply(x)` → `th.border.gamma_multiply(x)` (panel.rs 515/601). Skip the `IconKind::…tint()` else-branches (not tokens). `bell_pulse` sites already done in Task 3.
- [ ] **Step 3: Build + run the two touched test modules** — `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 20` (clean), then `cargo test --target-dir target/agent settings:: 2>&1 | Select-Object -Last 15` and `cargo test --target-dir target/agent confirm:: 2>&1 | Select-Object -Last 15` → PASS (these test modules are token-free, so they only need to compile).
- [ ] **Step 4: Commit** — `git add src/settings.rs src/panel.rs src/confirm.rs && git commit -m "refactor(panel,settings,confirm): read colors via theme::live seam"` (+ trailer)

### Task 7: Migrate `settings_menu.rs` consumers (clean)

**Files:** Modify `src/settings_menu.rs`.

- [ ] **Step 1:** In `show`, `draw_rail`, `draw_pane`, `draw_control`, add `let th = crate::theme::live(ui.ctx());` at each fn top. Replace tokens at the mapped lines (`WIN_BG/TEXT/TITLE_BG_FOCUS/BORDER/DIM/SEL_BG/BORDER_FOCUS/BELL`). Do **not** touch the local geometry consts `WIN_W/RAIL_W/TITLE_H/BODY_H/FOOTER_H/PANE_MIN_W`.
- [ ] **Step 2: Build + run** — `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 20` (clean); `cargo test --target-dir target/agent settings_menu:: 2>&1 | Select-Object -Last 15` → PASS.
- [ ] **Step 3: Commit** — `git add src/settings_menu.rs && git commit -m "refactor(settings_menu): read colors via theme::live seam"` (+ trailer)

### Task 8: Migrate `main.rs::show_os_chrome` (clean)

**Files:** Modify `src/main.rs`.

- [ ] **Step 1:** In `App::show_os_chrome` (takes `ctx: &egui::Context`), add `let th = crate::theme::live(ctx);` at the top. Replace `APP_BORDER`→`th.app_border()`, `CHROME_BG`→`th.chrome_bg`, `CHROME_BORDER`→`th.chrome_border`, `CHROME_BTN_HOVER`→`th.chrome_btn_hover`, `CHROME_CLOSE_HOVER`→`th.chrome_close_hover`, `DIM`→`th.dim`, `TEXT`→`th.text` at lines 203/312/315/322/331/334/336/340. Leave `APP_BORDER_W` (a width const, not in theme.rs).
- [ ] **Step 2: Build** — `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 20` → clean.
- [ ] **Step 3: Commit** — `git add src/main.rs && git commit -m "refactor(main): OS chrome reads colors via theme::live"` (+ trailer)

### Task 9: Parameterize the `terminal.rs` color-resolution pipeline

This is the load-bearing task: the pure resolver fns have no ctx (one runs off the egui thread), and they ripple to `frame.rs` and `inspect.rs`.

**Files:** Modify `src/terminal.rs`, `src/frame.rs`, `src/inspect.rs`.

**Interfaces:**
- Produces: `pub(crate) struct GridColors { pub fg: Color32, pub bg: Color32, pub palette: [Color32; 16] }` with `impl GridColors { pub fn from_theme(t: &crate::theme::Theme) -> Self; pub fn default_warm() -> Self }`. New signatures: `indexed_rgb(i: u8, gc: &GridColors)`, `pub(crate) fn resolve(c: AnsiColor, gc: &GridColors) -> Option<Color32>`, `pub(crate) fn glyph_style(flags, fg, bg, gc: &GridColors) -> GlyphStyle`, `fn query_color(index: usize, gc: &GridColors)`.

- [ ] **Step 1: Write the failing test** (pipeline honors a swapped palette; default path unchanged)

```rust
#[test]
fn resolve_uses_the_supplied_palette_not_a_const() {
    let mut gc = GridColors::default_warm();
    gc.palette[1] = egui::Color32::from_rgb(1, 2, 3); // recolor ANSI red
    assert_eq!(resolve(ansi_indexed(1), &gc), Some(egui::Color32::from_rgb(1, 2, 3)));
    // default fg fallback still comes from gc.fg
    assert_eq!(glyph_style(GlyphFlags::empty(), None, None, &gc).fg, gc.fg);
}
```
(Use the file's existing helpers to build an `AnsiColor::Indexed(1)`; mirror `query_color_maps_palette_and_named_slots`.)

- [ ] **Step 2: Run to verify it fails** — `cargo test --target-dir target/agent terminal::tests::resolve_uses 2>&1 | Select-Object -Last 15` → FAIL.

- [ ] **Step 3: Implement.** Add `GridColors` (with `from_theme` copying `t.fg/t.bg/t.palette`, and `default_warm` = `from_theme(&Theme::foreman_warm())`). Thread `&GridColors` through `indexed_rgb`/`resolve`/`glyph_style`/`query_color`, replacing `PALETTE[i]`→`gc.palette[i]`, `FG`→`gc.fg`, `BG`→`gc.bg`. Update callers:
  - `terminal.rs::show` (has ctx): `let gc = GridColors::from_theme(&crate::theme::live(ui.ctx()));` then pass `&gc` to `glyph_style`/`resolve`. Migrate `show`/`paint_search_bar`'s own 16 ctx-clean token refs to `th.<field>` in the same pass (`th` = the live theme; `gc` for the pipeline).
  - `frame.rs:152,213` (`glyph_style(cell.flags, cell.fg, cell.bg)`): add a `gc: &GridColors` param to the enclosing plan-builder fn and pass `&gc` through; its caller in `terminal.rs::show` supplies `GridColors::from_theme(&live)`.
  - `inspect.rs:155-156` (headless `--attrs`, no ctx): use `let gc = GridColors::default_warm();` and `resolve(cell.fg, &gc).unwrap_or(gc.fg)`. (Documented: headless inspection reports the default palette.)
  - `query_color` (off the egui thread via `Listener`): the `Session`/`Listener` captures a `GridColors` (for phase 3, `GridColors::default_warm()`); pass it in. (Documented gap: OSC 10/11/12 & OSC 4;N answers report the default palette, not the live theme — a phase-3b follow-up.)
  - `terminal.rs` `#[cfg(test)]` (9 refs) + the `default_style()` helper: build a `GridColors::default_warm()` (or read `Theme::default()` fields) instead of the bare consts.

- [ ] **Step 4: Run to verify it passes** — `cargo test --target-dir target/agent terminal:: 2>&1 | Select-Object -Last 25` → PASS (incl. `query_color_maps_palette_and_named_slots`, `glyph_style_*`). Then `cargo test --target-dir target/agent frame:: 2>&1 | Select-Object -Last 15` and `inspect::` → PASS.

- [ ] **Step 5: Commit** — `git add src/terminal.rs src/frame.rs src/inspect.rs && git commit -m "refactor(terminal): parameterize the color-resolution pipeline with GridColors"` (+ trailer)

### Task 10: Privatize the color consts (completeness gate) + prove byte-identity

**Files:** Modify `src/theme.rs`.

- [ ] **Step 1:** Remove `pub` from every **color** const (`BG`…`PALETTE`, `CHAT_COLORS`, etc.) so they are module-private (only `foreman_warm()`/`GridColors::default_warm` use them). Keep `pub` on `BELL_PERIOD` (config.rs:189 reads it), `bell_pulse`, and `unmultiplied` only if still referenced externally (grep — likely both can also become private). Keep the `use crate::theme::*` globs in consumers (they still resolve `bell_pulse`/`BELL_PERIOD`).
- [ ] **Step 2: Build — the compiler now enumerates any missed consumer** — `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 30`. Fix each `E0603 private constant` / unresolved-name by migrating that straggler to `th.<field>` or `gc.*`. Repeat until clean.
- [ ] **Step 3: Full suite** — `cargo test --target-dir target/agent 2>&1 | Select-Object -Last 15` → ~708 passing (re-run a lone flaky once).
- [ ] **Step 4: Prove byte-identity (visual gate).** Build a **second instance** (`target/agent/debug/foreman.exe` — never kill the host), screenshot the desktop, a project with 2+ terminals, chat open, and the settings window; compare against equivalents captured from `main` @ `0939487`. Must be pixel-identical. `Read` the PNGs to confirm.
- [ ] **Step 5: Commit** — `git add src/theme.rs && git commit -m "refactor(theme): privatize color consts; consts now define the default only"` (+ trailer)

---

## Stage C — Appearance pane (colors), live-apply

### Task 11: `AppearanceView` model (working/saved/dirty/revert)

**Files:**
- Create: `src/appearance.rs` (pure model + view; mirrors `settings.rs`'s `SettingsView` split)
- Modify: `src/main.rs` (add `mod appearance;`)
- Test: `src/appearance.rs` tests

**Interfaces:**
- Produces: `pub struct AppearanceView { working: Theme, saved: Theme, active_name: String, presets: Vec<String> }` with `pub fn new() -> Self`, `pub fn set_active(&mut self, name: &str, theme: Theme)`, `pub fn is_dirty(&self) -> bool` (`working != saved`), `pub fn revert(&mut self)` (`working = saved.clone()`), and `pub fn working(&self) -> &Theme`. `pub enum Outcome { Changed, Duplicate(String), Close, Pending }`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn dirty_tracks_edits_and_revert_restores() {
    let mut v = AppearanceView::new();
    v.set_active("Foreman Warm", Theme::foreman_warm());
    assert!(!v.is_dirty());
    v.working_mut().bg = egui::Color32::from_rgb(9, 9, 9);
    assert!(v.is_dirty());
    v.revert();
    assert!(!v.is_dirty());
    assert_eq!(v.working().bg, Theme::foreman_warm().bg);
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test --target-dir target/agent appearance:: 2>&1 | Select-Object -Last 15` → FAIL.
- [ ] **Step 3: Implement** the struct + methods (`working_mut()` for the test/UI). No egui yet.
- [ ] **Step 4: Run to verify it passes** → PASS.
- [ ] **Step 5: Commit** — `git add src/appearance.rs src/main.rs && git commit -m "feat(appearance): AppearanceView model (working/saved/dirty/revert)"` (+ trailer)

### Task 12: App owns `active_theme`; seed each frame + read-back (live-apply, no save yet)

**Files:** Modify `src/main.rs`.

**Interfaces:**
- Consumes: `theme::seed_live`/`live`. Produces: `App.active_theme: Arc<Theme>` field (+ `App.theme_dirty_at: Option<Instant>` for Stage D).

- [ ] **Step 1:** Add `active_theme: std::sync::Arc<crate::theme::Theme>` to `App` (init `Arc::new(Theme::foreman_warm())` in `App::new`, near `settings: Settings::load()` at line 138) and `theme_dirty_at: Option<std::time::Instant>` (init `None`).
- [ ] **Step 2:** Seed it each frame — right after `config::seed_live(&ctx, &self.settings)` at line 552: `crate::theme::seed_live(&ctx, &self.active_theme);`.
- [ ] **Step 3:** Read it back after the WM render (in the settings-diff region ~648): if `*crate::theme::live(&ctx) != *self.active_theme { self.active_theme = crate::theme::live(&ctx); self.theme_dirty_at = Some(Instant::now()); }`. (Save is Task 18; here the read-back just makes edits live-apply and marks dirty.)
- [ ] **Step 4: Build** — `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 20` → clean (the field is seeded but nothing edits it yet — behavior unchanged).
- [ ] **Step 5: Commit** — `git add src/main.rs && git commit -m "feat(main): App owns active_theme, seeds + reads it back each frame"` (+ trailer)

### Task 13: Wire `Pane::Appearance` into `settings_menu.rs` + thread `&mut Theme`

**Files:** Modify `src/settings_menu.rs`, `src/wm.rs`.

- [ ] **Step 1: Write the failing test** (the pane exists and is custom-body)

```rust
// in settings_menu.rs tests
#[test]
fn appearance_is_a_custom_body_pane() {
    assert!(Pane::ALL.contains(&Pane::Appearance));
    assert!(rows(Pane::Appearance).is_empty());        // custom body, like Keybindings
    assert_eq!(Pane::Appearance.label(), "Appearance");
}
```
Also widen the existing `every_pane_has_rows_and_labels` (line ~1104) to `if p != Pane::Keybindings && p != Pane::Appearance`.

- [ ] **Step 2: Run to verify it fails** — `cargo test --target-dir target/agent settings_menu::tests::appearance 2>&1 | Select-Object -Last 15` → FAIL.
- [ ] **Step 3: Implement the wiring** (all in `settings_menu.rs` unless noted):
  1. `enum Pane`: add `Appearance,`.
  2. `Pane::ALL`: add `Pane::Appearance` (choose slot; put it first, above `Terminal`) and bump `[Pane; 6]` → `[Pane; 7]`.
  3. `label()`: `Pane::Appearance => "Appearance",`.
  4. `rows()`: `Pane::Appearance => &[],` (mirror the Keybindings arm at 178).
  5. `SettingsMenu` struct: add `pub appearance: AppearanceView` next to `keybindings` (379); init `appearance: AppearanceView::new()` in `new()` (397).
  6. `show()` signature (507): add `theme: &mut crate::theme::Theme` after `km: &mut Keymap`; thread it into the `draw_pane(..)` call (568) and `draw_pane`'s signature (752).
  7. `draw_pane`: after the `just_entered` take (764) and mirroring the Keybindings block (765), add:
     ```rust
     if self.pane == Pane::Appearance {
         let reads_input = active && !self.in_rail && !just_entered;
         match self.appearance.show(ui, rect, reads_input, theme) {
             appearance::Outcome::Changed => bump(outcome, MenuOutcome::Changed),
             appearance::Outcome::Duplicate(name) => { /* Task 19 handles create+switch */ bump(outcome, MenuOutcome::Changed); let _ = name; }
             appearance::Outcome::Close => self.in_rail = true,
             appearance::Outcome::Pending => {}
         }
         return;
     }
     ```
  8. `handle_keys`: before the Esc-close check (625), add the twin gate `if self.pane == Pane::Appearance && !self.in_rail { if tab { self.in_rail = true; } return MenuOutcome::Pending; }`.
  9. `wm.rs` `Content::Settings` arm (177): add `let mut th = (*crate::theme::live(ui.ctx())).clone();`, extend the call to `menu.show(ui, rect, active, &mut live, &mut km, &mut th)`, and on `MenuOutcome::Changed` add `crate::theme::seed_live(ui.ctx(), &th);` beside the existing config/keymap reseeds. (The App reads it back — Task 12.)
- [ ] **Step 4: Run to verify it passes** — `cargo test --target-dir target/agent settings_menu:: 2>&1 | Select-Object -Last 20` → PASS; `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 20` → clean.
- [ ] **Step 5: Commit** — `git add src/settings_menu.rs src/wm.rs && git commit -m "feat(settings): add custom-body Appearance pane; thread live Theme through"` (+ trailer)

### Task 14: `AppearanceView::show` — split layout + live preview + palette grid

**Files:** Modify `src/appearance.rs`.

**Interfaces:**
- Produces: `pub fn show(&mut self, ui: &mut egui::Ui, rect: egui::Rect, reads_input: bool, out_theme: &mut Theme) -> Outcome`. On any control change it copies `self.working` into `*out_theme` and returns `Outcome::Changed` (the ctx round-trip in wm+App applies it live).

- [ ] **Step 1:** Implement the two-column split: left = a scrollable control column (`rect` left ~55%), right = a sticky preview column (right ~45%). Draw into child UIs via `ui.new_child`/`ui.painter_at`, mirroring how `SettingsMenu::show`/`SettingsView::show` split a rect. Only read pointer/keyboard when `reads_input` (the phase-2 CRITICAL was an unfocused pane reading input — do not repeat it).
- [ ] **Step 2:** Implement `fn paint_preview(ui: &mut egui::Ui, rect: egui::Rect, t: &Theme)` — a self-contained sample (NO PTY): fill `rect` with `t.bg`; paint a prompt line + a command in `t.fg`; a few output tokens using `t.palette[1..7]`; a selected span washed with `t.selection`; a filled caret block in `t.caret`; a 1px focus frame in `t.border_focus`. Below it, a 16-swatch ANSI grid drawing each `t.palette[i]`. Draw with `t = self.working` so the preview reflects in-progress edits.
- [ ] **Step 3:** After building the columns, if `self.is_dirty()` show a **Revert to saved** affordance in the left column footer (calls `self.revert()` and returns `Changed`).
- [ ] **Step 4: Build + visual check** — `cargo build --target-dir target/agent 2>&1 | Select-Object -Last 20` (clean). Second-instance screenshot: open settings (`Ctrl+B` then `Ctrl+,`), select **Appearance**; confirm the split renders (controls left, sample + palette right). `Read` the PNG.
- [ ] **Step 5: Commit** — `git add src/appearance.rs && git commit -m "feat(appearance): split layout with live sample preview + palette grid"` (+ trailer)

### Task 15: Color pickers (core set) wired to the working theme, live-apply

**Files:** Modify `src/appearance.rs`.

- [ ] **Step 1:** In the left column, add labeled controls (only when `reads_input`, collect `changed`):
  - Opaque tokens — `ui.color_edit_button_srgb(&mut rgb)` where `rgb: [u8;3]` decomposed from / recomposed to `self.working.<field>` (`bg`, `fg`, `border_focus`, and each `palette[i]` swatch built in a `ui.push_id(i, …)` loop so the 16 popups get distinct ids).
  - Translucent tokens — `selection`, `caret` — via `ui.color_edit_button_srgba_unmultiplied(&mut rgba4)`, holding the working `[u8;4]` straight-alpha in `AppearanceView` state for the currently-edited token (recompute from `Color32` only when the active token changes) to dodge the low-alpha premultiplied round-trip drift. Recompose to `Color32` via `theme::unmultiplied(r,g,b,a)`.
  - A **preset** `ComboBox` listing `self.presets` (built-in + user names) and a **Duplicate [name]** button returning `Outcome::Duplicate(new_name)`.
  - The existing **font-size** stepper (read/write `terminal::font_size`/`set_font_size` on `ui.ctx()`, same as the Terminal pane).
- [ ] **Step 2:** If any control `.changed()`, `*out_theme = self.working.clone()` and return `Outcome::Changed`.
- [ ] **Step 3: Build + visual check (live-apply)** — second instance: edit **background**; confirm the preview **and the real terminals behind the settings window** repaint immediately; edit `palette[1]` and confirm red text in a running terminal changes. `Read` the PNGs.
- [ ] **Step 4: Commit** — `git add src/appearance.rs && git commit -m "feat(appearance): color pickers live-apply to the working theme"` (+ trailer)

---

## Stage D — user themes + persistence

### Task 16: `themes_dir()` + dir-parameterized JSON helpers

**Files:** Modify `src/config.rs`.

**Interfaces:**
- Produces: `pub fn themes_dir() -> Option<PathBuf>`; `pub fn load_json_from<T: DeserializeOwned + Default>(dir: &Path, file: &str) -> T`; `pub fn save_json_in<T: Serialize>(dir: &Path, file: &str, value: &T) -> Result<(), String>`. Refactor the existing `load_json`/`save_json` to delegate to the `_from`/`_in` variants with `config_dir()`.

- [ ] **Step 1: Write the failing test** — round-trip a small serde struct through `save_json_in`/`load_json_from` into a `tempfile`-style dir (use `std::env::temp_dir().join(...)` unique per test; clean up).
- [ ] **Step 2: Run to verify it fails** → FAIL (unresolved fns).
- [ ] **Step 3: Implement** `themes_dir()` (`config_dir()?.join("themes")`, `create_dir_all`, `Some`), the `_from`/`_in` variants (move the bodies of `load_json`/`save_json`, parameterizing the dir; keep the atomic tmp+rename), and delegate.
- [ ] **Step 4: Run to verify it passes** → PASS. `cargo test --target-dir target/agent config:: 2>&1 | Select-Object -Last 15`.
- [ ] **Step 5: Commit** — `git add src/config.rs && git commit -m "feat(config): themes_dir + dir-parameterized json helpers"` (+ trailer)

### Task 17: `Settings.theme` field + default + name-sanitize

**Files:** Modify `src/config.rs`. Test: `src/config.rs` tests.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn theme_defaults_to_foreman_warm_and_sanitizes_empty() {
    assert_eq!(Settings::default().theme, "Foreman Warm");
    let mut s = Settings::default(); s.theme = String::new(); s.sanitize();
    assert_eq!(s.theme, "Foreman Warm"); // empty self-heals
}
```
Also extend the existing `new_fields_default_when_missing_from_old_file` to assert `theme == "Foreman Warm"`.

- [ ] **Step 2: Run to verify it fails** → FAIL.
- [ ] **Step 3: Implement:** add `pub const DEFAULT_THEME: &str = "Foreman Warm";` near `DEFAULT_FONT_SIZE`; add `pub theme: String` as the last `Settings` field (after `update_check`); set `theme: DEFAULT_THEME.into()` in `Default`; in `sanitize()`, `if self.theme.trim().is_empty() { self.theme = DEFAULT_THEME.into(); }` (full unknown-name validation lands in Task 19 once the theme list exists). No change to `load()`/`save()` — theme rides settings.json.
- [ ] **Step 4: Run to verify it passes** → PASS.
- [ ] **Step 5: Commit** — `git add src/config.rs && git commit -m "feat(config): Settings.theme name field + default + sanitize"` (+ trailer)

### Task 18: `Theme::load(name)` / `save(name)`; App loads + debounce-saves

**Files:** Modify `src/theme.rs`, `src/main.rs`. Test: `src/theme.rs` tests.

**Interfaces:**
- Produces: `impl Theme { pub fn is_builtin(name: &str) -> bool; pub fn load(name: &str) -> Theme; pub fn save(&self, name: &str) -> Result<(), String>; pub fn user_theme_names() -> Vec<String> }`. Built-in `"Foreman Warm"` is code-only (never written).

- [ ] **Step 1: Write the failing test** — `save("Test Theme", &edited)` then `load("Test Theme")` equals `edited`; `load("Foreman Warm")` equals `foreman_warm()`; `load("does-not-exist")` falls back to `foreman_warm()`; `is_builtin("Foreman Warm")` is true.
- [ ] **Step 2: Run to verify it fails** → FAIL.
- [ ] **Step 3: Implement:** `load(name)` → if built-in, `foreman_warm()`; else `config::load_json_from(themes_dir, "<slug>.json")` (falls back to `Theme::default()` on missing/corrupt — the `serde(default)` + tolerant loader). `save(name)` → refuse built-in (return `Ok(())` or an error; never write), else `config::save_json_in(themes_dir, "<slug>.json", self)`. `user_theme_names()` → list `*.json` in `themes_dir`. Slug = filesystem-safe form of the name.
- [ ] **Step 4:** In `App::new`, after `settings` loads, set `self.active_theme = Arc::new(Theme::load(&self.settings.theme))`. In the frame loop, flush the debounce (mirror the font/settings debounce at 642-678): when `theme_dirty_at` elapses `FONT_SAVE_DEBOUNCE`, if `!Theme::is_builtin(&self.settings.theme)` then `self.active_theme.save(&self.settings.theme)` (log on error), clear `theme_dirty_at`. Reload `active_theme` when `self.settings.theme` (the name) changes.
- [ ] **Step 5: Run to verify it passes + build** → PASS; `cargo build --target-dir target/agent` clean. **Commit** — `git add src/theme.rs src/main.rs && git commit -m "feat(theme): user-theme load/save + App debounced persistence"` (+ trailer)

### Task 19: Duplicate action + built-in read-only + preset list

**Files:** Modify `src/appearance.rs`, `src/settings_menu.rs`, `src/config.rs`.

- [ ] **Step 1:** Populate `AppearanceView.presets` from `["Foreman Warm"]` + `Theme::user_theme_names()` (refresh on `set_active`). When the active theme `Theme::is_builtin`, **disable the color controls** (draw them read-only / greyed) so the built-in can't be mutated in place; only Duplicate is active.
- [ ] **Step 2:** Handle `Outcome::Duplicate(new_name)` in `settings_menu`/`wm`/`App`: create a user theme = current `working`, `Theme::save(&new_name, &working)`, set `settings.theme = new_name` (persists via settings.json debounce), reload `active_theme`, and `set_active` so the new theme is now editable.
- [ ] **Step 3:** Extend `Settings::sanitize()` (or a startup check) so an unknown `theme` name (not built-in and no file) resets to `DEFAULT_THEME`.
- [ ] **Step 4: Build + visual check** — second instance: with **Foreman Warm** active, confirm color pickers are disabled; **Duplicate** → a new editable theme; edit it; restart the second instance → the edited theme reloads (persisted). `Read` the PNGs.
- [ ] **Step 5: Commit** — `git add src/appearance.rs src/settings_menu.rs src/config.rs && git commit -m "feat(appearance): Duplicate builtin→user theme; builtin read-only; preset list"` (+ trailer)

### Task 20: Corruption tolerance + full-suite green

**Files:** Test-only additions in `src/theme.rs` / `src/config.rs`.

- [ ] **Step 1: Write tests:** a `themes/*.json` with a bad hex token loads via `serde(default)` to the built-in value for that field (rest intact); a truncated/invalid file loads as `Theme::default()`; `save`→`load` round-trips a fully-edited theme.
- [ ] **Step 2: Run** — `cargo test --target-dir target/agent theme:: config:: 2>&1 | Select-Object -Last 20` → PASS.
- [ ] **Step 3: Full suite** — `cargo test --target-dir target/agent 2>&1 | Select-Object -Last 15` → ~708+ passing (re-run a lone flaky once).
- [ ] **Step 4: Commit** — `git add src/theme.rs src/config.rs && git commit -m "test(theme): user-theme corruption tolerance + round-trip"` (+ trailer)

---

## Stage E — review, docs, verification

### Task 21: `foreman-reviewer` pass on the load-bearing diff

- [ ] **Step 1:** Invoke the `foreman-reviewer` agent on the seam + input-reading pane diff: `src/theme.rs`, `src/appearance.rs`, the `terminal.rs` `GridColors` pipeline, the `settings_menu.rs` Appearance wiring, and the `main.rs`/`wm.rs` seed/read-back. Focus: the DSR/Listener `query_color` off-thread path, `reads_input` gating (unfocused pane must be inert), byte-identity of the default, and borrow discipline in the pickers.
- [ ] **Step 2:** Triage findings; fix CRITICAL/Important inline (each fix = its own TDD cycle + commit); doc-note MINORs.

### Task 22: Docs

- [ ] **Step 1:** Update `docs/settings-menu.md` — add the Appearance pane (split preview, live-apply, Duplicate/Revert, built-in read-only).
- [ ] **Step 2:** Update `src/theme.rs` module doc — the seam; consts now define the built-in default; drop the "static by design" caveat.
- [ ] **Step 3:** Write `docs/theme-system.md` (grug-simple): what it does, the seam, the user-theme file format (hex tokens, `%APPDATA%\foreman\themes\`), and the two documented gaps (headless `--attrs` and OSC color-answers report the default palette — phase-3b). Include a **Key files** section.
- [ ] **Step 4:** Update the phase-1 spec status header and the auto-memory (`settings-menu-design.md`) to mark phase 3 landed.
- [ ] **Step 5: Commit** — `git add docs/ src/theme.rs && git commit -m "docs(theme): document the theme system + Appearance pane"` (+ trailer)

### Task 23: Final visual verification pass

- [ ] **Step 1:** Second-instance screenshots covering the Definition of Done: (a) default theme renders identically to `main`; (b) Appearance split-preview; (c) editing bg/palette repaints preview + real terminals live; (d) Duplicate→edit→restart reloads the saved theme; (e) Revert restores; (f) built-in controls disabled. `Read` each PNG and confirm.
- [ ] **Step 2:** Report evidence; then invoke `superpowers:finishing-a-development-branch` (the user has consistently chosen **merge-to-main locally**, fast-forward, no push — offer that plus PR/keep).

---

## Self-Review (completed against the spec)

- **Spec coverage:** Theme struct (T1) ✓; seam (T2) ✓; consumer migration incl. the ctx-less pipeline (T3–T10) ✓; Appearance split-preview + live-apply + Revert (T11–T15) ✓; user themes + persistence + Duplicate + built-in read-only (T16–T20) ✓; hex serialization (T1) ✓; docs + reviewer + visual (T21–T23) ✓. Deferred per spec: font family / line spacing / cursor shape+blink (phase 3b); scheme import / direct-edit / bell sound (phase 4).
- **Type consistency:** `GridColors { fg, bg, palette }`, `theme::live(ctx) -> Arc<Theme>`, `AppearanceView::show(ui, rect, reads_input, &mut Theme) -> Outcome`, `menu.show(..., &mut Theme)`, `Theme::{load,save,is_builtin,user_theme_names}` — used consistently across tasks.
- **Documented gaps (intentional, colors-first):** headless `--attrs` (`inspect.rs`) and OSC color-answers (`query_color`, off-thread) report the **default** palette, not the live theme — flagged in T9 and documented in T22.
- **Placeholder scan:** none — every code step carries real code; mechanical migrations give the grep pattern + the exact mapped line list + known false-positives.
