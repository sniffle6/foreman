//! Every named color token in one place — the theme, statically.
//!
//! Deliberately NOT a runtime theme system: exactly one theme exists, so a
//! switchable Theme struct would be interface with nothing behind it (same
//! deferral reasoning as the chat-storage trait). When a second real theme
//! lands, these consts become fields on a struct — a mechanical upgrade,
//! because every consumer already goes through this module.

use eframe::egui;

/// Const port of `Color32::from_rgba_unmultiplied` (not a `const fn` upstream).
/// Integer rounding matches the float path for every token below — verify by
/// eye if you add one with a new alpha.
const fn unmultiplied(r: u8, g: u8, b: u8, a: u8) -> egui::Color32 {
    const fn mul(c: u8, a: u8) -> u8 {
        ((c as u32 * a as u32 + 127) / 255) as u8
    }
    egui::Color32::from_rgba_premultiplied(mul(r, a), mul(g, a), mul(b, a), a)
}

// ---- surfaces ----
/// The terminal surface color. Every window body paints this so the reserved
/// (unfilled) title bands at both levels blend into the content below them.
const BG: egui::Color32 = egui::Color32::from_rgb(20, 18, 15);
const DESK_BG: egui::Color32 = egui::Color32::from_rgb(25, 23, 19);
const WIN_BG: egui::Color32 = egui::Color32::from_rgb(33, 30, 24);
const TITLE_BG: egui::Color32 = egui::Color32::from_rgb(43, 39, 31);
const TITLE_BG_FOCUS: egui::Color32 = egui::Color32::from_rgb(56, 49, 36);

// ---- text ----
const TEXT: egui::Color32 = egui::Color32::from_rgb(222, 222, 212);
const DIM: egui::Color32 = egui::Color32::from_rgb(150, 143, 125);
/// Terminal default foreground. Same value as `TEXT` today, kept as its own
/// token because a theme may want grid text and UI text to diverge.
const FG: egui::Color32 = egui::Color32::from_rgb(222, 222, 212);

// ---- borders & focus ----
// Selection reads from border brightness: focused terminal white, focused
// project a step dimmer so the two levels stay distinct at a glance even
// with thin borders, everything else dark (same brightness ladder as the
// old amber scheme, re-based on neutral white).
const BORDER: egui::Color32 = egui::Color32::from_rgb(60, 60, 60);
const BORDER_FOCUS: egui::Color32 = egui::Color32::from_rgb(231, 231, 231);
const PROJ_BORDER_FOCUS: egui::Color32 = egui::Color32::from_rgb(150, 150, 150);

// ---- selection & caret ----
/// Translucent white over selected terminal cells.
const SELECTION: egui::Color32 = unmultiplied(231, 231, 231, 70);
/// `selection.bg_fill` for TextEdits (rename fields).
const SELECTION_TEXT_BG: egui::Color32 = unmultiplied(231, 231, 231, 90);
/// Selected-row wash (settings editor, landing recents). Straight alpha via
/// `unmultiplied` — `from_rgba_premultiplied` with rgb > alpha is an invalid
/// premultiplied color and blends as a near-white additive band.
const SEL_BG: egui::Color32 = unmultiplied(231, 231, 231, 30);
const CARET: egui::Color32 = unmultiplied(231, 169, 63, 130);
/// Scrim painted over an unfocused pane's grid when `Settings::dim_unfocused`
/// is on, so the focused pane reads clearly amid several visible siblings.
const DIM_UNFOCUSED: egui::Color32 = egui::Color32::from_black_alpha(46);
/// Bell attention pulse — the caret amber family at full alpha (CARET's 130
/// alpha is tuned for a block fill; a 1px border stroke needs full strength).
/// Never the focus ladder: Bell is amber and temporary, focus is near-white.
const BELL: egui::Color32 = egui::Color32::from_rgb(231, 169, 63);
/// One full breathe of the Bell pulse, seconds.
pub const BELL_PERIOD: f64 = 1.2;

/// Bell pulse animation: breathe [`BELL`] between ~40% and full strength on a
/// `period`-second cycle (settings `bell_period`; [`BELL_PERIOD`] is the
/// documented default). Pure function of wall-clock seconds (egui
/// `input.time`) so every pulsing surface breathes in sync.
pub fn bell_pulse(t: f64, period: f64, bell: egui::Color32) -> egui::Color32 {
    let phase = 0.5 + 0.5 * (t * std::f64::consts::TAU / period).sin();
    bell.gamma_multiply(0.4 + 0.6 * phase as f32)
}
/// Scrollback indicator thumb at a pane's right edge.
const SCROLL_THUMB: egui::Color32 = unmultiplied(231, 231, 231, 150);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bell_pulse_breathes_within_the_bell_color() {
        // Peak (sin=1 at t=P/4) is full BELL; trough (t=3P/4) is dimmer but
        // never black — the pulse must stay visible at its low point.
        let peak = bell_pulse(BELL_PERIOD / 4.0, BELL_PERIOD, BELL);
        let trough = bell_pulse(3.0 * BELL_PERIOD / 4.0, BELL_PERIOD, BELL);
        assert_eq!(peak, BELL, "peak of the breathe is the full bell color");
        assert!(trough.r() < BELL.r(), "trough must dim");
        assert!(trough.r() > 60, "trough must stay clearly visible");
        // Same hue family throughout: warm, R > G > B.
        assert!(trough.r() > trough.g() && trough.g() > trough.b());
    }

    #[test]
    fn custom_period_shifts_the_peak_by_the_same_phase() {
        // Same phase (t/period) → same color, whatever the period.
        assert_eq!(bell_pulse(0.5, 2.0, BELL), bell_pulse(0.25, 1.0, BELL));
    }

    #[test]
    fn foreman_warm_equals_the_legacy_consts() {
        // The runtime default is built FROM the consts, so it must render
        // byte-identically to the historical static palette.
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
        // Opaque → #rrggbb; premultiplied-with-alpha → #rrggbbaa; both exact,
        // including the raw-premultiplied SNAP_FILL that has no straight-alpha form.
        for c in [BG, PALETTE[3], SNAP_FILL, SELECTION, CARET] {
            let s = color_hex::to_hex(c);
            assert_eq!(color_hex::from_hex(&s).unwrap(), c, "round-trip {s}");
        }
        assert_eq!(color_hex::to_hex(BG), "#14120f");
        assert!(color_hex::from_hex("#zzz").is_err());
    }

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

    #[test]
    fn missing_tokens_default_from_the_builtin() {
        // Forward-compat: a file missing newly-added tokens still loads — the
        // container `serde(default)` fills every absent field from the built-in.
        let t: Theme = serde_json::from_str("{}").unwrap();
        assert_eq!(t, Theme::default());
    }

    #[test]
    fn a_partial_file_keeps_present_tokens_and_defaults_the_rest() {
        let t: Theme = serde_json::from_str(r##"{"bg":"#010203"}"##).unwrap();
        assert_eq!(t.bg, egui::Color32::from_rgb(1, 2, 3));
        assert_eq!(t.fg, Theme::default().fg);
    }

    #[test]
    fn a_bad_token_is_rejected_so_the_loader_falls_back() {
        // color_hex rejects malformed hex and a short palette array; the tolerant
        // loader (load_json_from) turns that Err into Theme::default() — i.e. a
        // corrupt theme file resolves to the built-in rather than bricking the UI.
        assert!(serde_json::from_str::<Theme>(r##"{"bg":"#zzz"}"##).is_err());
        assert!(serde_json::from_str::<Theme>(r##"{"palette":["#010203"]}"##).is_err());
    }

    #[test]
    fn a_fully_edited_theme_round_trips_through_json() {
        let mut t = Theme::foreman_warm();
        t.bg = egui::Color32::from_rgb(9, 8, 7);
        t.palette[5] = egui::Color32::from_rgb(1, 2, 3);
        t.selection = unmultiplied(10, 20, 30, 44); // a translucent token
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Theme>(&json).unwrap(), t);
    }

    #[test]
    fn slug_is_filesystem_safe() {
        assert_eq!(slug("Foreman Warm copy"), "foreman-warm-copy");
        assert_eq!(slug("Test  Theme!!"), "test-theme-");
    }

    #[test]
    fn user_theme_save_load_round_trips_and_builtin_is_readonly() {
        // The built-in is code-only: recognized, loads to foreman_warm, never written.
        assert!(Theme::is_builtin(crate::appearance::BUILTIN));
        assert_eq!(
            Theme::load(crate::appearance::BUILTIN),
            Theme::foreman_warm()
        );
        assert!(
            Theme::foreman_warm()
                .save(crate::appearance::BUILTIN)
                .is_ok(),
            "saving the built-in name is a no-op success"
        );

        // A uniquely-named user theme round-trips through the real themes_dir()
        // (unique slug so a real user theme is never clobbered). Clean up after.
        let name = "Zz Task18 Roundtrip";
        let file = crate::config::themes_dir().map(|d| d.join(format!("{}.json", slug(name))));
        if let Some(ref f) = file {
            let _ = std::fs::remove_file(f);
        }
        let mut edited = Theme::foreman_warm();
        edited.bg = egui::Color32::from_rgb(1, 2, 3);
        edited.palette[1] = egui::Color32::from_rgb(9, 9, 9);
        edited.save(name).unwrap();
        assert_eq!(
            Theme::load(name),
            edited,
            "saved user theme loads back exactly"
        );

        // A non-built-in name with no file falls back to the built-in default.
        assert_eq!(
            Theme::load("Zz No Such Theme Task18"),
            Theme::foreman_warm()
        );

        if let Some(ref f) = file {
            let _ = std::fs::remove_file(f);
        }
    }
}

// ---- scrollback search ----
/// Ordinary match highlight (distinct from selection).
const SEARCH_MATCH: egui::Color32 = unmultiplied(96, 143, 176, 90);
/// Current/focused match highlight (brighter than ordinary).
const SEARCH_CURRENT: egui::Color32 = unmultiplied(231, 169, 63, 120);
/// Search bar background (top-right overlay).
const SEARCH_BAR_BG: egui::Color32 = egui::Color32::from_rgb(43, 39, 31);
/// Search bar border.
const SEARCH_BAR_BORDER: egui::Color32 = egui::Color32::from_rgb(80, 74, 60);
/// Invalid-regex text in the search bar.
const SEARCH_ERROR: egui::Color32 = egui::Color32::from_rgb(214, 102, 84);

// ---- app chrome (hover-revealed OS bar + window frame) ----
const CHROME_BG: egui::Color32 = egui::Color32::from_rgb(42, 42, 42);
const CHROME_BORDER: egui::Color32 = egui::Color32::from_rgb(58, 58, 58);
const CHROME_BTN_HOVER: egui::Color32 = egui::Color32::from_rgb(66, 66, 66);
const CHROME_CLOSE_HOVER: egui::Color32 = egui::Color32::from_rgb(196, 43, 28);

// ---- accents ----
const DANGER: egui::Color32 = egui::Color32::from_rgb(214, 102, 84);
// snap overlay (amber, web mockup --needs #e7a93f)
const SNAP_FILL: egui::Color32 = egui::Color32::from_rgba_premultiplied(231, 169, 63, 33); // ~13% alpha
const SNAP_STROKE: egui::Color32 = egui::Color32::from_rgb(231, 169, 63);

// ---- chat viewer ----
// Sender colors are assigned by terminal-id hash — stable for a given id,
// distinct enough across a small fleet.
const CHAT_COLORS: [egui::Color32; 6] = [
    egui::Color32::from_rgb(231, 169, 63), // amber (also the human "you")
    egui::Color32::from_rgb(127, 179, 127), // green
    egui::Color32::from_rgb(111, 167, 199), // blue
    egui::Color32::from_rgb(199, 127, 174), // pink
    egui::Color32::from_rgb(180, 160, 100), // sand
    egui::Color32::from_rgb(140, 170, 160), // sage
];
const CHAT_STALE: egui::Color32 = egui::Color32::from_rgb(202, 164, 90);
const CHAT_LIVE: egui::Color32 = egui::Color32::from_rgb(127, 179, 127);
const CHAT_EDGE: egui::Color32 = egui::Color32::from_rgb(150, 107, 28);
const CHAT_MENTION_BG: egui::Color32 = egui::Color32::from_rgb(69, 64, 47);

// ---- ANSI 16-color palette ----
const PALETTE: [egui::Color32; 16] = [
    egui::Color32::from_rgb(43, 40, 36),
    egui::Color32::from_rgb(207, 91, 72),
    egui::Color32::from_rgb(148, 163, 109),
    egui::Color32::from_rgb(231, 169, 63),
    egui::Color32::from_rgb(96, 143, 176),
    egui::Color32::from_rgb(176, 122, 161),
    egui::Color32::from_rgb(116, 176, 164),
    egui::Color32::from_rgb(204, 198, 184),
    egui::Color32::from_rgb(111, 106, 93),
    egui::Color32::from_rgb(226, 97, 59),
    egui::Color32::from_rgb(174, 189, 127),
    egui::Color32::from_rgb(240, 197, 96),
    egui::Color32::from_rgb(122, 167, 199),
    egui::Color32::from_rgb(199, 155, 184),
    egui::Color32::from_rgb(143, 199, 187),
    egui::Color32::from_rgb(236, 231, 218),
];

use serde::{Deserialize, Serialize};

/// Runtime theme: every color token as a field. [`Theme::foreman_warm`] is the
/// built-in default, built from the module consts, so a default `Theme` renders
/// byte-identically to the historical static palette. Published each frame into
/// egui ctx data by `App` ([`seed_live`]); consumers read [`live`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)] // a missing token in a user file falls back to the built-in value
pub struct Theme {
    #[serde(with = "color_hex")]
    pub bg: egui::Color32,
    #[serde(with = "color_hex")]
    pub desk_bg: egui::Color32,
    #[serde(with = "color_hex")]
    pub win_bg: egui::Color32,
    #[serde(with = "color_hex")]
    pub title_bg: egui::Color32,
    #[serde(with = "color_hex")]
    pub title_bg_focus: egui::Color32,
    #[serde(with = "color_hex")]
    pub text: egui::Color32,
    #[serde(with = "color_hex")]
    pub dim: egui::Color32,
    #[serde(with = "color_hex")]
    pub fg: egui::Color32,
    #[serde(with = "color_hex")]
    pub border: egui::Color32,
    #[serde(with = "color_hex")]
    pub border_focus: egui::Color32,
    #[serde(with = "color_hex")]
    pub proj_border_focus: egui::Color32,
    #[serde(with = "color_hex")]
    pub selection: egui::Color32,
    #[serde(with = "color_hex")]
    pub selection_text_bg: egui::Color32,
    #[serde(with = "color_hex")]
    pub sel_bg: egui::Color32,
    #[serde(with = "color_hex")]
    pub caret: egui::Color32,
    #[serde(with = "color_hex")]
    pub dim_unfocused: egui::Color32,
    #[serde(with = "color_hex")]
    pub bell: egui::Color32,
    #[serde(with = "color_hex")]
    pub scroll_thumb: egui::Color32,
    #[serde(with = "color_hex")]
    pub search_match: egui::Color32,
    #[serde(with = "color_hex")]
    pub search_current: egui::Color32,
    #[serde(with = "color_hex")]
    pub search_bar_bg: egui::Color32,
    #[serde(with = "color_hex")]
    pub search_bar_border: egui::Color32,
    #[serde(with = "color_hex")]
    pub search_error: egui::Color32,
    #[serde(with = "color_hex")]
    pub chrome_bg: egui::Color32,
    #[serde(with = "color_hex")]
    pub chrome_border: egui::Color32,
    #[serde(with = "color_hex")]
    pub chrome_btn_hover: egui::Color32,
    #[serde(with = "color_hex")]
    pub chrome_close_hover: egui::Color32,
    #[serde(with = "color_hex")]
    pub danger: egui::Color32,
    #[serde(with = "color_hex")]
    pub snap_fill: egui::Color32,
    #[serde(with = "color_hex")]
    pub snap_stroke: egui::Color32,
    #[serde(with = "color_hex")]
    pub chat_stale: egui::Color32,
    #[serde(with = "color_hex")]
    pub chat_live: egui::Color32,
    #[serde(with = "color_hex")]
    pub chat_edge: egui::Color32,
    #[serde(with = "color_hex")]
    pub chat_mention_bg: egui::Color32,
    #[serde(with = "color_hex_array")]
    pub chat_colors: [egui::Color32; 6],
    #[serde(with = "color_hex_array")]
    pub palette: [egui::Color32; 16],
}

impl Theme {
    /// The built-in default, defined by the module consts (single source of
    /// truth — no duplicated literals, so the default is byte-identical to the
    /// historical static palette by construction).
    pub fn foreman_warm() -> Self {
        Self {
            bg: BG,
            desk_bg: DESK_BG,
            win_bg: WIN_BG,
            title_bg: TITLE_BG,
            title_bg_focus: TITLE_BG_FOCUS,
            text: TEXT,
            dim: DIM,
            fg: FG,
            border: BORDER,
            border_focus: BORDER_FOCUS,
            proj_border_focus: PROJ_BORDER_FOCUS,
            selection: SELECTION,
            selection_text_bg: SELECTION_TEXT_BG,
            sel_bg: SEL_BG,
            caret: CARET,
            dim_unfocused: DIM_UNFOCUSED,
            bell: BELL,
            scroll_thumb: SCROLL_THUMB,
            search_match: SEARCH_MATCH,
            search_current: SEARCH_CURRENT,
            search_bar_bg: SEARCH_BAR_BG,
            search_bar_border: SEARCH_BAR_BORDER,
            search_error: SEARCH_ERROR,
            chrome_bg: CHROME_BG,
            chrome_border: CHROME_BORDER,
            chrome_btn_hover: CHROME_BTN_HOVER,
            chrome_close_hover: CHROME_CLOSE_HOVER,
            danger: DANGER,
            snap_fill: SNAP_FILL,
            snap_stroke: SNAP_STROKE,
            chat_stale: CHAT_STALE,
            chat_live: CHAT_LIVE,
            chat_edge: CHAT_EDGE,
            chat_mention_bg: CHAT_MENTION_BG,
            chat_colors: CHAT_COLORS,
            palette: PALETTE,
        }
    }

    /// The app frame matches the revealed OS bar — derived, never stored.
    pub fn app_border(&self) -> egui::Color32 {
        self.chrome_bg
    }

    /// The built-in theme is code-only (its name is [`crate::appearance::BUILTIN`]):
    /// it never has a file, is never written, and always resolves to
    /// [`foreman_warm`](Self::foreman_warm).
    pub fn is_builtin(name: &str) -> bool {
        name == crate::appearance::BUILTIN
    }

    /// Resolve a theme by name. The built-in returns [`foreman_warm`](Self::foreman_warm);
    /// a user theme loads from `<slug>.json` under [`crate::config::themes_dir`],
    /// tolerantly falling back to [`Theme::default`] (the built-in) on a missing
    /// or corrupt file (via `serde(default)` + the tolerant loader).
    pub fn load(name: &str) -> Theme {
        if Self::is_builtin(name) {
            return Self::foreman_warm();
        }
        match crate::config::themes_dir() {
            Some(d) => crate::config::load_json_from(&d, &format!("{}.json", slug(name))),
            None => Theme::default(),
        }
    }

    /// Persist this theme under `name` as `<slug>.json` in [`crate::config::themes_dir`].
    /// The built-in is never written (returns `Ok(())`) — duplicating it is how a
    /// user theme is born.
    pub fn save(&self, name: &str) -> Result<(), String> {
        if Self::is_builtin(name) {
            return Ok(());
        }
        crate::config::themes_dir()
            .ok_or_else(|| "no themes dir".to_string())
            .and_then(|d| crate::config::save_json_in(&d, &format!("{}.json", slug(name)), self))
    }

    /// The names (file stems) of every user theme file in [`crate::config::themes_dir`].
    /// Best-effort: an empty vec when the dir is unavailable. Names are the slugs
    /// for now (display can be the slug).
    pub fn user_theme_names() -> Vec<String> {
        let Some(dir) = crate::config::themes_dir() else {
            return Vec::new();
        };
        let mut names = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        names.push(stem.to_string());
                    }
                }
            }
        }
        names
    }
}

/// Filesystem-safe file stem for a theme name: lowercase, every run of
/// non-alphanumeric characters collapsed to a single `-` (the caller appends
/// `.json`). e.g. `"Foreman Warm copy"` -> `"foreman-warm-copy"`. `save`/`load`
/// are symmetric on this, so a name round-trips regardless of trailing dashes.
pub(crate) fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out
}

impl Default for Theme {
    fn default() -> Self {
        Self::foreman_warm()
    }
}

/// Publish the active theme into egui ctx data for this frame — the same seam as
/// [`crate::config::seed_live`]/[`crate::keymap::seed_live`]. `App` calls this
/// each frame; every consuming fn reads it back via [`live`]. `insert_temp` is
/// cleared each frame, so this must run every frame.
pub fn seed_live(ctx: &egui::Context, t: &Theme) {
    let arc = std::sync::Arc::new(t.clone());
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("foreman::theme"), arc));
}

/// Read the theme published this frame by [`seed_live`]. Before the first seed
/// (or in a headless ctx) this returns the built-in default.
pub fn live(ctx: &egui::Context) -> std::sync::Arc<Theme> {
    ctx.data_mut(|d| d.get_temp(egui::Id::new("foreman::theme")))
        .unwrap_or_else(|| std::sync::Arc::new(Theme::default()))
}

/// Serde codec: `Color32` <-> `#rrggbb` (opaque) / `#rrggbbaa` (with alpha).
/// Encodes the stored *premultiplied* bytes verbatim so every token — including
/// the raw-premultiplied `SNAP_FILL`, which has no straight-alpha form —
/// round-trips exactly. `from_hex` is tolerant (bad input is an error, so a
/// corrupt file falls back to the built-in default via the tolerant loader).
pub mod color_hex {
    use super::egui::Color32;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn to_hex(c: Color32) -> String {
        let [r, g, b, a] = c.to_array();
        if a == 255 {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
        }
    }

    pub fn from_hex(s: &str) -> Result<Color32, String> {
        let h = s
            .strip_prefix('#')
            .ok_or_else(|| format!("missing #: {s}"))?;
        let byte = |i: usize| {
            h.get(i..i + 2)
                .ok_or_else(|| format!("short hex: {s}"))
                .and_then(|p| u8::from_str_radix(p, 16).map_err(|e| e.to_string()))
        };
        match h.len() {
            6 => Ok(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?)),
            8 => Ok(Color32::from_rgba_premultiplied(
                byte(0)?,
                byte(2)?,
                byte(4)?,
                byte(6)?,
            )),
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

    pub fn serialize<S: Serializer, const N: usize>(
        a: &[Color32; N],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let v: Vec<String> = a.iter().map(|c| super::color_hex::to_hex(*c)).collect();
        v.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        d: D,
    ) -> Result<[Color32; N], D::Error> {
        let v = Vec::<String>::deserialize(d)?;
        let mut out = [Color32::BLACK; N];
        for (i, slot) in out.iter_mut().enumerate() {
            let s = v
                .get(i)
                .ok_or_else(|| serde::de::Error::custom("short color array"))?;
            *slot = super::color_hex::from_hex(s).map_err(serde::de::Error::custom)?;
        }
        Ok(out)
    }
}
