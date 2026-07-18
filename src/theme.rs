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
pub const BG: egui::Color32 = egui::Color32::from_rgb(20, 18, 15);
pub const DESK_BG: egui::Color32 = egui::Color32::from_rgb(25, 23, 19);
pub const WIN_BG: egui::Color32 = egui::Color32::from_rgb(33, 30, 24);
pub const TITLE_BG: egui::Color32 = egui::Color32::from_rgb(43, 39, 31);
pub const TITLE_BG_FOCUS: egui::Color32 = egui::Color32::from_rgb(56, 49, 36);

// ---- text ----
pub const TEXT: egui::Color32 = egui::Color32::from_rgb(222, 222, 212);
pub const DIM: egui::Color32 = egui::Color32::from_rgb(150, 143, 125);
/// Terminal default foreground. Same value as `TEXT` today, kept as its own
/// token because a theme may want grid text and UI text to diverge.
pub const FG: egui::Color32 = egui::Color32::from_rgb(222, 222, 212);

// ---- borders & focus ----
// Selection reads from border brightness: focused terminal white, focused
// project a step dimmer so the two levels stay distinct at a glance even
// with thin borders, everything else dark (same brightness ladder as the
// old amber scheme, re-based on neutral white).
pub const BORDER: egui::Color32 = egui::Color32::from_rgb(60, 60, 60);
pub const BORDER_FOCUS: egui::Color32 = egui::Color32::from_rgb(231, 231, 231);
pub const PROJ_BORDER_FOCUS: egui::Color32 = egui::Color32::from_rgb(150, 150, 150);

// ---- selection & caret ----
/// Translucent white over selected terminal cells.
pub const SELECTION: egui::Color32 = unmultiplied(231, 231, 231, 70);
/// `selection.bg_fill` for TextEdits (rename fields).
pub const SELECTION_TEXT_BG: egui::Color32 = unmultiplied(231, 231, 231, 90);
/// Selected-row wash (settings editor, landing recents). Straight alpha via
/// `unmultiplied` — `from_rgba_premultiplied` with rgb > alpha is an invalid
/// premultiplied color and blends as a near-white additive band.
pub const SEL_BG: egui::Color32 = unmultiplied(231, 231, 231, 30);
pub const CARET: egui::Color32 = unmultiplied(231, 169, 63, 130);
/// Bell attention pulse — the caret amber family at full alpha (CARET's 130
/// alpha is tuned for a block fill; a 1px border stroke needs full strength).
/// Never the focus ladder: Bell is amber and temporary, focus is near-white.
pub const BELL: egui::Color32 = egui::Color32::from_rgb(231, 169, 63);
/// One full breathe of the Bell pulse, seconds.
pub const BELL_PERIOD: f64 = 1.2;

/// Bell pulse animation: breathe [`BELL`] between ~40% and full strength on a
/// `period`-second cycle (settings `bell_period`; [`BELL_PERIOD`] is the
/// documented default). Pure function of wall-clock seconds (egui
/// `input.time`) so every pulsing surface breathes in sync.
pub fn bell_pulse(t: f64, period: f64) -> egui::Color32 {
    let phase = 0.5 + 0.5 * (t * std::f64::consts::TAU / period).sin();
    BELL.gamma_multiply(0.4 + 0.6 * phase as f32)
}
/// Scrollback indicator thumb at a pane's right edge.
pub const SCROLL_THUMB: egui::Color32 = unmultiplied(231, 231, 231, 150);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bell_pulse_breathes_within_the_bell_color() {
        // Peak (sin=1 at t=P/4) is full BELL; trough (t=3P/4) is dimmer but
        // never black — the pulse must stay visible at its low point.
        let peak = bell_pulse(BELL_PERIOD / 4.0, BELL_PERIOD);
        let trough = bell_pulse(3.0 * BELL_PERIOD / 4.0, BELL_PERIOD);
        assert_eq!(peak, BELL, "peak of the breathe is the full bell color");
        assert!(trough.r() < BELL.r(), "trough must dim");
        assert!(trough.r() > 60, "trough must stay clearly visible");
        // Same hue family throughout: warm, R > G > B.
        assert!(trough.r() > trough.g() && trough.g() > trough.b());
    }

    #[test]
    fn custom_period_shifts_the_peak_by_the_same_phase() {
        // Same phase (t/period) → same color, whatever the period.
        assert_eq!(bell_pulse(0.5, 2.0), bell_pulse(0.25, 1.0));
    }
}

// ---- scrollback search ----
/// Ordinary match highlight (distinct from selection).
pub const SEARCH_MATCH: egui::Color32 = unmultiplied(96, 143, 176, 90);
/// Current/focused match highlight (brighter than ordinary).
pub const SEARCH_CURRENT: egui::Color32 = unmultiplied(231, 169, 63, 120);
/// Search bar background (top-right overlay).
pub const SEARCH_BAR_BG: egui::Color32 = egui::Color32::from_rgb(43, 39, 31);
/// Search bar border.
pub const SEARCH_BAR_BORDER: egui::Color32 = egui::Color32::from_rgb(80, 74, 60);
/// Invalid-regex text in the search bar.
pub const SEARCH_ERROR: egui::Color32 = egui::Color32::from_rgb(214, 102, 84);

// ---- app chrome (hover-revealed OS bar + window frame) ----
pub const CHROME_BG: egui::Color32 = egui::Color32::from_rgb(42, 42, 42);
pub const CHROME_BORDER: egui::Color32 = egui::Color32::from_rgb(58, 58, 58);
pub const CHROME_BTN_HOVER: egui::Color32 = egui::Color32::from_rgb(66, 66, 66);
pub const CHROME_CLOSE_HOVER: egui::Color32 = egui::Color32::from_rgb(196, 43, 28);
pub const APP_BORDER: egui::Color32 = CHROME_BG; // frame matches the revealed bar

// ---- accents ----
pub const DANGER: egui::Color32 = egui::Color32::from_rgb(214, 102, 84);
// snap overlay (amber, web mockup --needs #e7a93f)
pub const SNAP_FILL: egui::Color32 = egui::Color32::from_rgba_premultiplied(231, 169, 63, 33); // ~13% alpha
pub const SNAP_STROKE: egui::Color32 = egui::Color32::from_rgb(231, 169, 63);

// ---- chat viewer ----
// Sender colors are assigned by terminal-id hash — stable for a given id,
// distinct enough across a small fleet.
pub const CHAT_COLORS: [egui::Color32; 6] = [
    egui::Color32::from_rgb(231, 169, 63), // amber (also the human "you")
    egui::Color32::from_rgb(127, 179, 127), // green
    egui::Color32::from_rgb(111, 167, 199), // blue
    egui::Color32::from_rgb(199, 127, 174), // pink
    egui::Color32::from_rgb(180, 160, 100), // sand
    egui::Color32::from_rgb(140, 170, 160), // sage
];
pub const CHAT_STALE: egui::Color32 = egui::Color32::from_rgb(202, 164, 90);
pub const CHAT_LIVE: egui::Color32 = egui::Color32::from_rgb(127, 179, 127);
pub const CHAT_EDGE: egui::Color32 = egui::Color32::from_rgb(150, 107, 28);
pub const CHAT_MENTION_BG: egui::Color32 = egui::Color32::from_rgb(69, 64, 47);

// ---- ANSI 16-color palette ----
pub const PALETTE: [egui::Color32; 16] = [
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
